//! `Classifier` and `ModelLoader` with ONNX Runtime and a Hugging Face tokenizer.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::Context;
use async_trait::async_trait;
use ort::session::Session;
use ort::session::SessionOutputs;
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
    async fn load(&self, dir: &Path, _device: Device) -> anyhow::Result<Arc<dyn Classifier>> {
        let dir = dir.to_owned();
        let model = tokio::task::spawn_blocking(move || Model::open(&dir)).await??;
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
    /// Opens the model folder `dir`.
    ///
    /// # Errors
    ///
    /// A file cannot be read or parsed.
    fn open(dir: &Path) -> anyhow::Result<Self> {
        let path = dir.join("labels.json");
        let text =
            fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let labels: Labels = serde_json::from_str(&text)?;
        let session = Session::builder()?.commit_from_file(dir.join("model.onnx"))?;
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
    /// `decode_bio` fails.
    fn prediction(
        &self,
        encoding: &Encoding,
        text: &str,
        intent_logits: &[f32],
        slot_logits: &[f32],
    ) -> anyhow::Result<Prediction> {
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

    use crate::domain::model::Slot;
    use crate::domain::training::Example;
    use crate::domain::training::Label;

    /// The fixture model folder.
    const FIXTURE: &str = "models/system-one/fixture";

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
}
