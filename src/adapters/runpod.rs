//! `Trainer` that runs the training image on `RunPod`, through the `RunPod` REST API and the
//! network volume.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use futures::TryStreamExt as _;
use object_store::ObjectMeta;
use object_store::ObjectStore;
use object_store::ObjectStoreExt as _;
use object_store::PutPayload;
use object_store::aws::AmazonS3Builder;
use object_store::path::Path as StorePath;
use serde_json::Value;
use serde_json::json;
use tokio::time;
use tokio::time::Instant;

use crate::RunPodConfig;
use crate::RunPodKeys;
use crate::domain::training::TrainingSet;
use crate::ports::Trainer;

/// The `RunPod` REST API.
const API: &str = "https://api.runpod.ai";

/// The longest time of one request, so that a stalled request returns to the deadline check.
const REQUEST: Duration = Duration::from_secs(30);

/// Trains with the `RunPod` handler of the training image.
pub struct RunPodTrainer {
    /// The HTTP client of all requests.
    client: reqwest::Client,
    /// The `RunPod` REST API.
    api: String,
    /// The serverless endpoint of the training image.
    endpoint_id: String,
    /// The API key.
    api_key: String,
    /// The network volume.
    store: Arc<dyn ObjectStore>,
    /// The folder of the System One model folders.
    dir: PathBuf,
    /// The time between two status requests.
    poll: Duration,
    /// The time after which the job is cancelled.
    timeout: Duration,
}

/// The volume folder of the run `run_id`.
fn folder(run_id: &str) -> StorePath {
    StorePath::from_iter(["jarvis", "runs", run_id])
}

impl RunPodTrainer {
    /// The trainer of the endpoint and the volume of `config`, with model folders in `dir`.
    ///
    /// # Errors
    ///
    /// The S3 settings are not valid, or the HTTP client cannot start.
    pub fn try_new(config: &RunPodConfig, keys: &RunPodKeys, dir: PathBuf) -> anyhow::Result<Self> {
        // Only production gets a request timeout: under the paused test clock it fires at once
        // while a real socket waits (`02-rules.md` section 6.2).
        let client = reqwest::Client::builder().timeout(REQUEST).build()?;
        let store = AmazonS3Builder::new()
            .with_endpoint(&config.s3_endpoint)
            .with_region(&config.s3_region)
            .with_bucket_name(&config.volume_id)
            .with_access_key_id(&keys.s3_key)
            .with_secret_access_key(&keys.s3_secret)
            .build()?;
        Ok(Self {
            client,
            api: API.to_owned(),
            endpoint_id: config.endpoint_id.clone(),
            api_key: keys.api_key.clone(),
            store: Arc::new(store),
            dir,
            poll: Duration::from_secs(config.poll_s),
            timeout: Duration::from_secs(config.timeout_min * 60),
        })
    }

    /// Sends a request to `<api>/v2/<endpoint_id>/<path>` with the API key and parses the reply.
    ///
    /// # Errors
    ///
    /// The request fails, or the reply has an error status or another form.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<Value> {
        let url = format!("{}/v2/{}/{path}", self.api, self.endpoint_id);
        let mut request = self.client.request(method, &url).bearer_auth(&self.api_key);
        if let Some(body) = body {
            request = request.json(&body);
        }
        Ok(request.send().await?.error_for_status()?.json().await?)
    }

    /// Cancels the job `id`. A failed cancel only logs, because the run already has an error.
    async fn cancel(&self, id: &str) {
        let cancel = format!("cancel/{id}");
        if let Err(error) = self.send(reqwest::Method::POST, &cancel, None).await {
            tracing::warn!("cannot cancel RunPod job {id}: {error:#}");
        }
    }

    /// Waits until the job `id` completes.
    ///
    /// # Errors
    ///
    /// A request fails, or the job fails, is cancelled or times out.
    async fn wait(&self, id: &str) -> anyhow::Result<()> {
        let deadline = Instant::now() + self.timeout;
        loop {
            anyhow::ensure!(Instant::now() < deadline, "RunPod job {id} timed out");
            let status = format!("status/{id}");
            let status = self.send(reqwest::Method::GET, &status, None).await?;
            let error = status["error"].as_str().unwrap_or_default();
            match status["status"].as_str() {
                Some("COMPLETED") => return Ok(()),
                Some(end @ ("FAILED" | "CANCELLED" | "TIMED_OUT")) => {
                    anyhow::bail!("RunPod job {id} {end}: {error}")
                }
                _ => time::sleep(self.poll).await,
            }
        }
    }

    /// Copies each file under `model` on the volume into the new folder `out`. On an error,
    /// `out` does not change.
    ///
    /// # Errors
    ///
    /// `out` is a folder with files, or a file cannot be read or written.
    async fn download(&self, model: &StorePath, out: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(&self.dir)?;
        // The temporary folder deletes itself on an error.
        let temporary = tempfile::tempdir_in(&self.dir)?;
        let objects: Vec<ObjectMeta> = self.store.list(Some(model)).try_collect().await?;
        for object in objects {
            let name = object.location.filename().context("no file name")?;
            let bytes = self.store.get(&object.location).await?.bytes().await?;
            fs::write(temporary.path().join(name), bytes)?;
        }
        fs::rename(temporary.path(), out)
            .with_context(|| format!("cannot create {}", out.display()))
    }

    /// Uploads `set`, runs the job of `run_id` and downloads its model files.
    ///
    /// # Errors
    ///
    /// The upload, a request, the job or the download fails.
    async fn run(&self, run_id: &str, set: &TrainingSet) -> anyhow::Result<PathBuf> {
        let data = PutPayload::from(serde_json::to_vec(set)?);
        self.store
            .put(&folder(run_id).join("train.json"), data)
            .await?;
        let input = json!({ "input": { "run_id": run_id } });
        let job: Value = self.send(reqwest::Method::POST, "run", Some(input)).await?;
        let id = job["id"].as_str().context("no job ID")?;
        if let Err(error) = self.wait(id).await {
            // The job can still run and write to the volume after the delete of `train`.
            self.cancel(id).await;
            return Err(error);
        }
        let out = self.dir.join(run_id);
        self.download(&folder(run_id).join("model"), &out).await?;
        Ok(out)
    }

    /// Deletes each object of the run `run_id` from the volume.
    ///
    /// # Errors
    ///
    /// The list or a delete fails.
    async fn delete(&self, run_id: &str) -> anyhow::Result<()> {
        let run = folder(run_id);
        let objects: Vec<ObjectMeta> = self.store.list(Some(&run)).try_collect().await?;
        for object in objects {
            self.store.delete(&object.location).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Trainer for RunPodTrainer {
    async fn train(&self, run_id: &str, set: &TrainingSet) -> anyhow::Result<PathBuf> {
        let result = self.run(run_id, set).await;
        // A left object costs only volume space, so it does not fail a trained run.
        if let Err(error) = self.delete(run_id).await {
            tracing::warn!("cannot delete run {run_id} from the RunPod volume: {error:#}");
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::OnceLock;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use futures::executor;
    use object_store::memory::InMemory;
    use tempfile::TempDir;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::Request;
    use wiremock::ResponseTemplate;
    use wiremock::matchers;

    /// The run ID of each test.
    const RUN: &str = "run-1";

    /// The job ID of each test.
    const JOB: &str = "job-1";

    /// The model files that the handler writes.
    const FILES: [&str; 3] = ["model.onnx", "tokenizer.json", "labels.json"];

    /// A mock `RunPod` API, its volume with the model files of the handler, and a trainer.
    struct Fixture {
        /// The mock `RunPod` API.
        server: MockServer,
        /// The volume.
        store: Arc<InMemory>,
        /// The `train.json` of the volume when the job starts.
        uploaded: Arc<OnceLock<Vec<u8>>>,
        /// The folder of the model folders.
        _folder: TempDir,
        /// The trainer under test.
        trainer: RunPodTrainer,
    }

    /// Mounts the mock of a new job, which keeps the `train.json` of the volume of `fixture`.
    async fn mount_run(fixture: &Fixture) {
        let (store, uploaded) = (Arc::clone(&fixture.store), Arc::clone(&fixture.uploaded));
        let path = folder(RUN).join("train.json");
        let run = move |_: &Request| {
            let get = async { store.get(&path).await?.bytes().await };
            if let Ok(bytes) = executor::block_on(get) {
                uploaded.get_or_init(|| bytes.to_vec());
            }
            ResponseTemplate::new(200).set_body_json(json!({ "id": JOB, "status": "IN_QUEUE" }))
        };
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/v2/endpoint/run"))
            .and(matchers::header("authorization", "Bearer key"))
            .and(matchers::body_json(json!({ "input": { "run_id": RUN } })))
            .respond_with(run)
            .expect(1)
            .mount(&fixture.server)
            .await;
    }

    /// Mounts on `server` the mock of the job status, which gives `statuses` in order.
    async fn mount_status(server: &MockServer, statuses: Vec<&'static str>) {
        let count = AtomicUsize::new(0);
        let status = move |_: &Request| {
            let index = count.fetch_add(1, Ordering::SeqCst).min(statuses.len() - 1);
            let error = "torch.OutOfMemoryError: CUDA out of memory";
            let body = json!({ "status": statuses[index], "error": error });
            ResponseTemplate::new(200).set_body_json(body)
        };
        Mock::given(matchers::method("GET"))
            .and(matchers::path(format!("/v2/endpoint/status/{JOB}")))
            .respond_with(status)
            .mount(server)
            .await;
    }

    /// Mounts on `server` the mock of the job cancel, which expects one request with the key.
    async fn mount_cancel(server: &MockServer) {
        Mock::given(matchers::method("POST"))
            .and(matchers::path(format!("/v2/endpoint/cancel/{JOB}")))
            .and(matchers::header("authorization", "Bearer key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": JOB })))
            .expect(1)
            .mount(server)
            .await;
    }

    /// A fixture whose job has `statuses` in order.
    async fn fixture(statuses: Vec<&'static str>) -> anyhow::Result<Fixture> {
        let (server, store) = (MockServer::start().await, Arc::new(InMemory::new()));
        let folder = tempfile::tempdir()?;
        for file in FILES {
            let path = super::folder(RUN).join("model").join(file);
            let bytes = PutPayload::from(file.as_bytes().to_vec());
            store.put(&path, bytes).await?;
        }
        let trainer = RunPodTrainer {
            client: reqwest::Client::new(),
            api: server.uri(),
            endpoint_id: "endpoint".to_owned(),
            api_key: "key".to_owned(),
            store: store.clone(),
            dir: folder.path().join("models"),
            poll: Duration::from_secs(30),
            timeout: Duration::from_secs(3600),
        };
        let fixture = Fixture {
            server,
            store,
            uploaded: Arc::default(),
            _folder: folder,
            trainer,
        };
        mount_run(&fixture).await;
        mount_status(&fixture.server, statuses).await;
        Ok(fixture)
    }

    /// An empty training set.
    fn set() -> TrainingSet {
        TrainingSet {
            base_model: "base".to_owned(),
            epochs: 1,
            batch_size: 1,
            labels: Vec::new(),
            tags: Vec::new(),
            examples: Vec::new(),
        }
    }

    /// The text of the error of a `train` that must fail.
    async fn train_error(fixture: &Fixture) -> anyhow::Result<String> {
        let Err(error) = fixture.trainer.train(RUN, &set()).await else {
            anyhow::bail!("a failed job gave a model folder");
        };
        Ok(format!("{error:#}"))
    }

    /// AC-13.8c-3.
    #[test]
    fn ac_13_8c_3_section_gives_poll_and_timeout() -> anyhow::Result<()> {
        let text = fs::read_to_string("jarvis.toml")?;
        let lines = text.lines().skip_while(|line| *line != "# [train.runpod]");
        let section: Vec<&str> = lines
            .skip(1)
            .map_while(|line| line.strip_prefix("# "))
            .collect();
        let config: RunPodConfig = toml::from_str(&section.join("\n"))?;
        let keys = RunPodKeys {
            api_key: "key".to_owned(),
            s3_key: "s3".to_owned(),
            s3_secret: "secret".to_owned(),
        };
        let trainer = RunPodTrainer::try_new(&config, &keys, PathBuf::from("models"))?;
        assert_eq!(trainer.poll, Duration::from_secs(30));
        assert_eq!(trainer.timeout, Duration::from_secs(3600));
        Ok(())
    }

    /// AC-13.8b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_13_8b_1_volume_has_the_set() -> anyhow::Result<()> {
        let fixture = fixture(vec!["COMPLETED"]).await?;
        fixture.trainer.train(RUN, &set()).await?;
        let uploaded = fixture
            .uploaded
            .get()
            .context("no train.json at the job start")?;
        let uploaded: Value = serde_json::from_slice(uploaded)?;
        assert_eq!(uploaded, serde_json::to_value(set())?);
        Ok(())
    }

    /// AC-13.8b-2.
    #[tokio::test(start_paused = true)]
    async fn ac_13_8b_2_run_has_run_id_and_key() -> anyhow::Result<()> {
        let fixture = fixture(vec!["COMPLETED"]).await?;
        fixture.trainer.train(RUN, &set()).await?;
        fixture.server.verify().await;
        Ok(())
    }

    /// AC-13.8b-3.
    #[tokio::test(start_paused = true)]
    async fn ac_13_8b_3_completed_job_gives_the_model_files() -> anyhow::Result<()> {
        let fixture = fixture(vec!["IN_QUEUE", "IN_PROGRESS", "COMPLETED"]).await?;
        let out = fixture.trainer.train(RUN, &set()).await?;
        assert_eq!(out, fixture.trainer.dir.join(RUN));
        for file in FILES {
            assert_eq!(fs::read_to_string(out.join(file))?, file);
        }
        Ok(())
    }

    /// AC-13.8b-4.
    #[tokio::test(start_paused = true)]
    async fn ac_13_8b_4_failed_job_gives_its_error() -> anyhow::Result<()> {
        let fixture = fixture(vec!["IN_PROGRESS", "FAILED"]).await?;
        let error = train_error(&fixture).await?;
        assert!(error.contains("CUDA out of memory"), "error: {error}");
        Ok(())
    }

    /// AC-13.8b-5.
    #[tokio::test(start_paused = true)]
    async fn ac_13_8b_5_long_job_is_cancelled() -> anyhow::Result<()> {
        let fixture = fixture(vec!["IN_PROGRESS"]).await?;
        mount_cancel(&fixture.server).await;
        let error = train_error(&fixture).await?;
        assert!(error.contains("timed out"), "error: {error}");
        fixture.server.verify().await;
        Ok(())
    }

    /// AC-13.8b-5: a status error after the job start also cancels the job.
    #[tokio::test(start_paused = true)]
    async fn ac_13_8b_5_status_error_cancels_the_job() -> anyhow::Result<()> {
        let fixture = fixture(vec!["IN_PROGRESS"]).await?;
        Mock::given(matchers::path(format!("/v2/endpoint/status/{JOB}")))
            .respond_with(ResponseTemplate::new(503))
            .with_priority(1)
            .mount(&fixture.server)
            .await;
        mount_cancel(&fixture.server).await;
        let error = train_error(&fixture).await?;
        assert!(error.contains("503"), "error: {error}");
        fixture.server.verify().await;
        Ok(())
    }

    /// AC-13.8b-6.
    #[tokio::test(start_paused = true)]
    async fn ac_13_8b_6_volume_run_folder_is_deleted() -> anyhow::Result<()> {
        for statuses in [vec!["COMPLETED"], vec!["FAILED"]] {
            let fixture = fixture(statuses).await?;
            let result = fixture.trainer.train(RUN, &set()).await;
            let left: Vec<ObjectMeta> =
                fixture.store.list(Some(&folder(RUN))).try_collect().await?;
            assert!(left.is_empty(), "after {result:?}: {left:?}");
        }
        Ok(())
    }
}
