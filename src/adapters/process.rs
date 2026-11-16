//! `Trainer` that runs `train/train.py` as a local process.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use tokio::io::AsyncReadExt as _;
use tokio::process::Child;
use tokio::process::Command;
use tokio::time;

use crate::Device;
use crate::domain::training::TrainingSet;
use crate::ports::Trainer;

/// The number of error output lines in the error of a failed run.
const TAIL: usize = 20;

/// Trains with `train/train.py` on this host.
#[derive(Clone, Debug)]
pub struct ProcessTrainer {
    /// The Python interpreter that runs the script.
    pub python: PathBuf,
    /// The folder of the System One model folders.
    pub dir: PathBuf,
    /// Where the script trains.
    pub device: Device,
    /// The GPU memory cap of the run, in GB.
    pub max_vram_gb: f64,
    /// The time after which the run stops.
    pub timeout: Duration,
}

impl ProcessTrainer {
    /// Starts the script on the set file `data`, with the model files in `out`.
    ///
    /// # Errors
    ///
    /// The interpreter cannot start.
    fn spawn(&self, data: &Path, out: &Path) -> anyhow::Result<Child> {
        let device = match self.device {
            Device::Cuda => "cuda",
            Device::Cpu => "cpu",
        };
        Command::new(&self.python)
            .arg("train/train.py")
            .arg("--data")
            .arg(data)
            .arg("--out")
            .arg(out)
            .args(["--device", device, "--max-vram-gb"])
            .arg(self.max_vram_gb.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot start {}", self.python.display()))
    }

    /// Writes `set` to `out/train.json` and runs the script on it, with the model files in `out`.
    ///
    /// # Errors
    ///
    /// The file cannot be written, the script cannot start, exits with an error or times out.
    async fn run(&self, out: &Path, set: &TrainingSet) -> anyhow::Result<()> {
        let data = out.join("train.json");
        fs::write(&data, serde_json::to_vec(set)?)?;
        let mut child = self.spawn(&data, out)?;
        let mut stderr = child.stderr.take().context("no error output")?;
        let mut errors = Vec::new();
        let wait = async { tokio::join!(child.wait(), stderr.read_to_end(&mut errors)) };
        let Ok((status, read)) = time::timeout(self.timeout, wait).await else {
            child.kill().await?;
            anyhow::bail!("train.py timed out after {:?}", self.timeout);
        };
        read?;
        let status = status?;
        anyhow::ensure!(status.success(), "train.py {status}: {}", tail(&errors));
        Ok(())
    }
}

/// The last `TAIL` lines of the error output `errors`.
fn tail(errors: &[u8]) -> String {
    let text = String::from_utf8_lossy(errors);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(TAIL)..].join("\n")
}

#[async_trait]
impl Trainer for ProcessTrainer {
    async fn train(&self, run_id: &str, set: &TrainingSet) -> anyhow::Result<PathBuf> {
        let out = self.dir.join(run_id);
        fs::create_dir_all(&out).with_context(|| format!("cannot create {}", out.display()))?;
        if let Err(error) = self.run(&out, set).await {
            fs::remove_dir_all(&out)
                .with_context(|| format!("cannot delete {} after: {error:#}", out.display()))?;
            return Err(error);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;

    /// The run ID of each test.
    const RUN: &str = "run-1";

    /// A temporary folder with a fake interpreter that runs the shell script `body`, and its
    /// trainer on `device`.
    fn trainer(body: &str, device: Device) -> anyhow::Result<(TempDir, ProcessTrainer)> {
        let folder = tempfile::tempdir()?;
        let python = folder.path().join("python");
        fs::write(&python, format!("#!/bin/sh\n{body}\n"))?;
        fs::set_permissions(&python, fs::Permissions::from_mode(0o755))?;
        let trainer = ProcessTrainer {
            python,
            dir: folder.path().join("models"),
            device,
            max_vram_gb: 4.5,
            timeout: Duration::from_secs(3600),
        };
        Ok((folder, trainer))
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

    /// Whether a process with `path` in its arguments runs.
    fn runs(path: &Path) -> anyhow::Result<bool> {
        Ok(Command::new("pgrep")
            .arg("-f")
            .arg(path)
            .status()?
            .success())
    }

    /// AC-13.7-1.
    #[tokio::test]
    async fn ac_13_7_1_exit_zero_gives_the_run_folder() -> anyhow::Result<()> {
        let (_folder, trainer) = trainer("exit 0", Device::Cpu)?;
        assert_eq!(trainer.train(RUN, &set()).await?, trainer.dir.join(RUN));
        Ok(())
    }

    /// AC-13.7-2.
    #[tokio::test]
    async fn ac_13_7_2_exit_one_gives_the_error_output() -> anyhow::Result<()> {
        let body = "echo 'torch.OutOfMemoryError: CUDA out of memory' >&2\nexit 1";
        let (_folder, trainer) = trainer(body, Device::Cpu)?;
        let Err(error) = trainer.train(RUN, &set()).await else {
            anyhow::bail!("a failed run gave a model folder");
        };
        let text = format!("{error:#}");
        assert!(text.contains("CUDA out of memory"), "error: {text}");
        Ok(())
    }

    /// AC-13.7-3.
    #[tokio::test]
    async fn ac_13_7_3_script_gets_device_and_vram_cap() -> anyhow::Result<()> {
        let (folder, trainer) = trainer("echo \"$@\" > \"$0.args\"", Device::Cuda)?;
        trainer.train(RUN, &set()).await?;
        let args = fs::read_to_string(folder.path().join("python.args"))?;
        assert!(
            args.contains("--device cuda --max-vram-gb 4.5"),
            "arguments: {args}"
        );
        Ok(())
    }

    /// AC-13.7-4.
    #[tokio::test(start_paused = true)]
    async fn ac_13_7_4_long_run_is_killed() -> anyhow::Result<()> {
        let (_folder, trainer) = trainer("while :; do :; done", Device::Cpu)?;
        let Err(error) = trainer.train(RUN, &set()).await else {
            anyhow::bail!("a killed run gave a model folder");
        };
        let text = format!("{error:#}");
        assert!(text.contains("timed out"), "error: {text}");
        assert!(!runs(&trainer.python)?, "the script still runs");
        Ok(())
    }

    /// AC-13.7-5.
    #[tokio::test]
    async fn ac_13_7_5_error_deletes_the_run_folder() -> anyhow::Result<()> {
        let (_folder, trainer) = trainer("touch \"$5/model.onnx\"\nexit 1", Device::Cpu)?;
        assert!(trainer.train(RUN, &set()).await.is_err(), "the run passed");
        let out = trainer.dir.join(RUN);
        assert!(!out.exists(), "{} exists", out.display());
        Ok(())
    }
}
