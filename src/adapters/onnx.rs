//! `Classifier` and `ModelLoader` with ONNX Runtime and a Hugging Face tokenizer.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::Context;
use async_trait::async_trait;
use ort::execution_providers::CPUExecutionProvider;
use ort::execution_providers::CUDAExecutionProvider;
use ort::session::Session;
use ort::session::SessionOutputs;
use ort::session::builder::SessionBuilder;
use ort::value::Tensor;
use serde::Deserialize;
use tokenizers::Encoding;
use tokenizers::Tokenizer;

use crate::Device;
use crate::domain::model;
use crate::domain::model::Prediction;
use crate::ports::Classifier;
use crate::ports::ModelLoader;

/// Loads the model folders that `train/train.py` writes.
pub struct OnnxLoader;

#[async_trait]
impl ModelLoader for OnnxLoader {
    async fn load(&self, dir: &Path, device: Device) -> anyhow::Result<Arc<dyn Classifier>> {
        let dir = dir.to_owned();
        let model =
            tokio::task::spawn_blocking(move || Model::open(&dir, builder(device)?)).await??;
        Ok(Arc::new(OnnxClassifier(Arc::new(model))))
    }
}

/// A `Classifier` that runs each prediction on a blocking thread.
struct OnnxClassifier(Arc<Model>);

#[async_trait]
impl Classifier for OnnxClassifier {
    async fn predict(&self, context: &str, text: &str) -> anyhow::Result<Prediction> {
        let model = Arc::clone(&self.0);
        let (context, text) = (context.to_owned(), text.to_owned());
        tokio::task::spawn_blocking(move || model.predict(&context, &text)).await?
    }
}

/// The contents of `labels.json`.
#[derive(Deserialize)]
struct Labels {
    /// The intent labels in `intent_logits` order.
    labels: Vec<String>,
    /// The slot tags in `slot_logits` order.
    tags: Vec<String>,
}

/// One model folder: the session, the tokenizer, the labels and the tags.
struct Model {
    /// The ONNX Runtime session. A run needs exclusive access.
    session: Mutex<Session>,
    /// The tokenizer of `tokenizer.json`.
    tokenizer: Tokenizer,
    /// The labels and the tags.
    labels: Labels,
}

impl Model {
    /// Opens the model folder `dir` with the session options of `builder`.
    ///
    /// # Errors
    ///
    /// A file cannot be read or parsed.
    fn open(dir: &Path, builder: SessionBuilder) -> anyhow::Result<Self> {
        let path = dir.join("labels.json");
        let text =
            fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let labels: Labels = serde_json::from_str(&text)?;
        let session = builder.commit_from_file(dir.join("model.onnx"))?;
        let tokenizer =
            Tokenizer::from_file(dir.join("tokenizer.json")).map_err(anyhow::Error::from_boxed)?;
        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            labels,
        })
    }

    /// The prediction for the pair `(context, text)`.
    ///
    /// # Errors
    ///
    /// The tokenizer or the session fails, or `decode_bio` fails.
    fn predict(&self, context: &str, text: &str) -> anyhow::Result<Prediction> {
        let encoding = self
            .tokenizer
            .encode((context, text), true)
            .map_err(anyhow::Error::from_boxed)?;
        let inputs = ort::inputs![
            "input_ids" => tensor(encoding.get_ids())?,
            "attention_mask" => tensor(encoding.get_attention_mask())?,
            "token_type_ids" => tensor(encoding.get_type_ids())?,
        ];
        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("a run panicked"))?;
        let outputs = session.run(inputs)?;
        self.prediction(
            &encoding,
            text,
            logits(&outputs, "intent_logits")?,
            logits(&outputs, "slot_logits")?,
        )
    }

    /// The prediction from the logits of the session for `encoding` of `text`.
    ///
    /// # Errors
    ///
    /// The logits are empty or not finite, or `decode_bio` fails.
    fn prediction(
        &self,
        encoding: &Encoding,
        text: &str,
        intent_logits: &[f32],
        slot_logits: &[f32],
    ) -> anyhow::Result<Prediction> {
        anyhow::ensure!(
            valid(intent_logits) && valid(slot_logits),
            "the session output is empty or not finite"
        );
        let ranked = model::ranked(&self.labels.labels, &model::softmax(intent_logits));
        let tokens = encoding
            .get_sequence_ids()
            .into_iter()
            .zip(encoding.get_offsets())
            .zip(slot_logits.chunks(self.labels.tags.len()));
        let mut offsets = Vec::new();
        let mut tag_ids = Vec::new();
        for ((sequence, offset), tag_logits) in tokens {
            if sequence == Some(1) {
                offsets.push(*offset);
                tag_ids.push(top(tag_logits));
            }
        }
        let slots = model::decode_bio(text, &offsets, &tag_ids, &self.labels.tags)
            .map_err(|error| anyhow::anyhow!("cannot decode the slots: {error:?}"))?;
        Ok(Prediction { ranked, slots })
    }
}

/// A session builder that registers the execution provider of `device`.
///
/// # Errors
///
/// ONNX Runtime cannot register the provider.
fn builder(device: Device) -> ort::Result<SessionBuilder> {
    let provider = match device {
        Device::Cuda => CUDAExecutionProvider::default().build(),
        Device::Cpu => CPUExecutionProvider::default().build(),
    };
    Session::builder()?.with_execution_providers([provider.error_on_failure()])
}

/// Whether `logits` is not empty and has only finite values.
fn valid(logits: &[f32]) -> bool {
    !logits.is_empty() && logits.iter().all(|logit| logit.is_finite())
}

/// The input tensor of shape `[1, n]` with the `n` token values `values`.
///
/// # Errors
///
/// ONNX Runtime cannot make the tensor.
fn tensor(values: &[u32]) -> ort::Result<Tensor<i64>> {
    let values: Vec<i64> = values.iter().copied().map(i64::from).collect();
    Tensor::from_array(([1, values.len()], values))
}

/// The values of the `f32` output `name` of `outputs`.
///
/// # Errors
///
/// The output is absent or is not an `f32` tensor.
fn logits<'outputs>(
    outputs: &'outputs SessionOutputs,
    name: &str,
) -> anyhow::Result<&'outputs [f32]> {
    let output = outputs.get(name).context(format!("no output {name}"))?;
    Ok(output.try_extract_tensor::<f32>()?.1)
}

/// The index of the largest of `logits`.
fn top(logits: &[f32]) -> usize {
    let largest = logits
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(right.1));
    largest.map_or(0, |(index, _)| index)
}

/// Tests of the ONNX adapter with the fixture model.
#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeSet;
    use std::time::Duration;
    use std::time::Instant;

    use crate::domain::model::Slot;
    use crate::domain::training::Example;
    use crate::domain::training::Label;

    /// The fixture model folder.
    const FIXTURE: &str = "models/system-one/fixture";

    /// A context that gives 32 tokens with `TEXT`.
    const CONTEXT: &str = "area: kitchen | habits: none";

    /// A text that gives 32 tokens with `CONTEXT`.
    const TEXT: &str = "turn on the lights in the kitchen and the living room and set the brightness to fifty percent in the office now";

    /// Serializes the latency tests, so that one test does not slow the other.
    static LATENCY: Mutex<()> = Mutex::new(());

    /// The fixture model on `device`.
    fn fixture(device: Device) -> anyhow::Result<Model> {
        Model::open(Path::new(FIXTURE), builder(device)?)
    }

    /// The encoding of `CONTEXT` and `TEXT` by the tokenizer of `model`.
    fn encoding(model: &Model) -> anyhow::Result<Encoding> {
        let encoding = model.tokenizer.encode((CONTEXT, TEXT), true);
        encoding.map_err(anyhow::Error::from_boxed)
    }

    /// The execution providers of the nodes that one prediction on `device` runs.
    fn providers(device: Device) -> anyhow::Result<BTreeSet<String>> {
        let folder = tempfile::tempdir()?;
        let builder = builder(device)?.with_profiling(folder.path().join("profile"))?;
        let mut model = Model::open(Path::new(FIXTURE), builder)?;
        model.predict(CONTEXT, TEXT)?;
        let session = model.session.get_mut().expect("no run panicked");
        let profile = fs::read_to_string(session.end_profiling()?)?;
        let events: Vec<serde_json::Value> = serde_json::from_str(&profile)?;
        let providers = events
            .iter()
            .filter_map(|event| event["args"]["provider"].as_str());
        Ok(providers.map(str::to_owned).collect())
    }

    /// The p99 time of `predict` on `device` with 32 tokens, after a warm-up.
    fn p99(device: Device) -> anyhow::Result<Duration> {
        let _only = LATENCY.lock().expect("no latency test panicked");
        let model = fixture(device)?;
        assert_eq!(encoding(&model)?.len(), 32, "token count of the input");
        for _ in 0..100 {
            model.predict(CONTEXT, TEXT)?;
        }
        let mut times = Vec::new();
        for _ in 0..1000 {
            let start = Instant::now();
            model.predict(CONTEXT, TEXT)?;
            times.push(start.elapsed());
        }
        times.sort_unstable();
        Ok(times[989])
    }

    /// Each seed example that is not held out, with its prediction by the fixture model.
    async fn predictions() -> anyhow::Result<Vec<(Example, Prediction)>> {
        let classifier = OnnxLoader.load(Path::new(FIXTURE), Device::Cpu).await?;
        let house = Path::new("src/testing/house.json");
        let set = crate::seed_examples(Path::new("jarvis.toml"), Some(house))?;
        let mut pairs = Vec::new();
        for example in set.examples {
            let prediction = classifier.predict(&example.context, &example.text).await?;
            pairs.push((example, prediction));
        }
        Ok(pairs)
    }

    /// Asserts that the top label is the intent of each example whose intent `wanted` accepts.
    async fn assert_top(wanted: fn(&str) -> bool) -> anyhow::Result<()> {
        for (example, prediction) in predictions().await? {
            if let Label::Intent(intent) = &example.label
                && wanted(intent)
            {
                let top = prediction.ranked.first().map(|pair| pair.0.as_str());
                assert_eq!(top, Some(intent.as_str()), "top label of {}", example.text);
            }
        }
        Ok(())
    }

    /// Asserts that the slots of each example contain the value of each of its `slot` spans.
    async fn assert_slot(slot: &str) -> anyhow::Result<()> {
        for (example, prediction) in predictions().await? {
            for span in example
                .spans
                .iter()
                .flatten()
                .filter(|span| span.slot == slot)
            {
                let expected = Slot {
                    name: slot.to_owned(),
                    value: example.text[span.start..span.end].to_owned(),
                };
                assert!(
                    prediction.slots.contains(&expected),
                    "{expected:?} is not in {:?} of {}",
                    prediction.slots,
                    example.text
                );
            }
        }
        Ok(())
    }

    /// AC-3.12-1.
    #[tokio::test]
    async fn ac_3_12_1_top_label_is_the_intent() -> anyhow::Result<()> {
        assert_top(|intent| intent != "unknown").await
    }

    /// AC-3.12-2.
    #[tokio::test]
    async fn ac_3_12_2_slots_contain_the_area() -> anyhow::Result<()> {
        assert_slot("area").await
    }

    /// AC-3.12-3.
    #[tokio::test]
    async fn ac_3_12_3_slots_contain_the_show() -> anyhow::Result<()> {
        assert_slot("show").await
    }

    /// AC-3.12-4.
    #[tokio::test]
    async fn ac_3_12_4_top_label_is_unknown() -> anyhow::Result<()> {
        assert_top(|intent| intent == "unknown").await
    }

    /// AC-3.12-11.
    #[tokio::test]
    async fn ac_3_12_11_long_text_is_truncated() -> anyhow::Result<()> {
        let classifier = OnnxLoader.load(Path::new(FIXTURE), Device::Cpu).await?;
        let text = "turn on the lights ".repeat(300);
        classifier
            .predict("area: none | habits: none", text.trim_end())
            .await?;
        Ok(())
    }

    /// AC-3.12b-1.
    #[test]
    fn ac_3_12b_1_cpu_uses_only_the_cpu_provider() -> anyhow::Result<()> {
        let expected = BTreeSet::from(["CPUExecutionProvider".to_owned()]);
        assert_eq!(providers(Device::Cpu)?, expected);
        Ok(())
    }

    /// AC-3.12b-2.
    #[test]
    #[ignore = "GPU host"]
    fn ac_3_12b_2_cpu_p99_is_less_than_15_ms() -> anyhow::Result<()> {
        let p99 = p99(Device::Cpu)?;
        assert!(p99 < Duration::from_millis(15), "CPU p99 is {p99:?}");
        Ok(())
    }

    /// AC-3.12b-3.
    #[test]
    #[ignore = "GPU host"]
    fn ac_3_12b_3_cuda_uses_cuda_and_p99_is_less_than_5_ms() -> anyhow::Result<()> {
        let providers = providers(Device::Cuda)?;
        assert!(
            providers.contains("CUDAExecutionProvider"),
            "providers are {providers:?}"
        );
        let p99 = p99(Device::Cuda)?;
        assert!(p99 < Duration::from_millis(5), "CUDA p99 is {p99:?}");
        Ok(())
    }

    /// AC-3.12b-4.
    #[test]
    fn ac_3_12b_4_bad_output_is_an_error() -> anyhow::Result<()> {
        let model = fixture(Device::Cpu)?;
        let encoding = encoding(&model)?;
        let intent = vec![0.0; model.labels.labels.len()];
        let slots = vec![0.0; encoding.len() * model.labels.tags.len()];
        let bad = [
            vec![],
            vec![f32::NAN; slots.len()],
            vec![f32::INFINITY; slots.len()],
        ];
        for logits in &bad {
            for (intent, slots) in [(logits, &slots), (&intent, logits)] {
                let error = model.prediction(&encoding, TEXT, intent, slots);
                let error = error.err().map(|error| error.to_string());
                let expected = "the session output is empty or not finite";
                assert_eq!(error.as_deref(), Some(expected), "{intent:?} and {slots:?}");
            }
        }
        Ok(())
    }
}
