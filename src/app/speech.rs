//! The `speak` use case, the audio cache and the pre-render of the pool responses.

use std::collections::BTreeSet;
use std::sync::Arc;

use dashmap::DashMap;
use serde_json::Map;
use serde_json::Value;

use crate::domain::catalog;
use crate::domain::responses;
use crate::domain::responses::Pools;
use crate::domain::speech::Audio;
use crate::ports::TextToSpeech;

/// Speaks texts from the audio cache, or with TTS when a text is not in the cache.
pub struct Speech {
    /// The synthesis of the texts that are not in the cache.
    tts: Arc<dyn TextToSpeech>,
    /// The audio of each pre-rendered text.
    cache: DashMap<String, Arc<Audio>>,
}

impl Speech {
    /// Speaks with `tts` and an empty cache.
    #[must_use]
    pub fn new(tts: Arc<dyn TextToSpeech>) -> Self {
        Self {
            tts,
            cache: DashMap::new(),
        }
    }

    /// The audio of `text`: the cached audio, else a new synthesis that the cache does not keep.
    ///
    /// # Errors
    ///
    /// The text is not in the cache and the synthesis fails.
    pub async fn speak(&self, text: &str) -> anyhow::Result<Arc<Audio>> {
        if let Some(audio) = self.cache.get(text) {
            return Ok(Arc::clone(&audio));
        }
        Ok(Arc::new(self.tts.synth(text).await?))
    }

    /// Synthesizes and caches each pool response whose only variable is `area`, once for each
    /// area name in `areas`.
    ///
    /// # Errors
    ///
    /// A template does not parse or render, or a synthesis fails.
    pub async fn prerender(&self, pools: &Pools, areas: &[String]) -> anyhow::Result<()> {
        for text in texts(pools, areas)? {
            let audio = self.tts.synth(&text).await?;
            self.cache.insert(text, Arc::new(audio));
        }
        Ok(())
    }
}

/// The text of each pool response whose only variable is `area`, rendered with each area name in
/// `areas` in lowercase, as `decide` renders it.
fn texts(pools: &Pools, areas: &[String]) -> anyhow::Result<BTreeSet<String>> {
    let allowed = BTreeSet::from(["area".to_owned()]);
    let mut texts = BTreeSet::new();
    let templates = pools.values().flat_map(|pool| &pool.responses);
    for template in templates {
        if catalog::unknown_variable(&template.text, &allowed)?.is_some() {
            continue;
        }
        for area in areas {
            let context = Map::from_iter([("area".to_owned(), Value::from(area.to_lowercase()))]);
            texts.insert(responses::render(&template.text, &context)?);
        }
    }
    Ok(texts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::responses::Pool;
    use crate::domain::responses::Response;
    use crate::testing::fakes::FakeTts;

    /// A use case over a new `FakeTts`, and that fake.
    fn speech() -> (Speech, Arc<FakeTts>) {
        let tts = Arc::new(FakeTts::default());
        (Speech::new(Arc::clone(&tts) as Arc<dyn TextToSpeech>), tts)
    }

    /// The pool `lights_on` with an `area` template and a template with no variable, and the
    /// pool `temperature_query` with an `entity` template.
    fn pools() -> Pools {
        let pool = |texts: &[&str]| Pool {
            responses: texts
                .iter()
                .map(|text| Response {
                    text: (*text).to_owned(),
                    uses: 0,
                })
                .collect(),
            last: None,
        };
        let lights = pool(&["Powering up the {{ area }} lights.", "One moment."]);
        let temperature = pool(&["It is {{ entity.state }} degrees in the {{ area }}."]);
        Pools::from([
            ("lights_on".to_owned(), lights),
            ("temperature_query".to_owned(), temperature),
        ])
    }

    /// Pre-renders `pools()` for the Living Room and the Office.
    async fn prerender(speech: &Speech) -> anyhow::Result<()> {
        let areas = ["Living Room".to_owned(), "Office".to_owned()];
        speech.prerender(&pools(), &areas).await
    }

    /// AC-6.3b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_6_3b_1_hit_returns_cached_audio_without_synth() -> anyhow::Result<()> {
        let (speech, tts) = speech();
        let audio = Arc::new(Audio {
            samples: vec![1, 2, 3],
            rate: 16_000,
        });
        speech
            .cache
            .insert("One moment.".to_owned(), Arc::clone(&audio));
        assert_eq!(speech.speak("One moment.").await?, audio);
        assert_eq!(tts.texts(), Vec::<String>::new(), "a hit called synth");
        Ok(())
    }

    /// AC-6.3b-2.
    #[tokio::test(start_paused = true)]
    async fn ac_6_3b_2_prerender_caches_each_area_text() -> anyhow::Result<()> {
        let (speech, _tts) = speech();
        prerender(&speech).await?;
        let keys: BTreeSet<String> = speech
            .cache
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        let expected = [
            "One moment.",
            "Powering up the living room lights.",
            "Powering up the office lights.",
        ];
        assert_eq!(keys, BTreeSet::from(expected.map(str::to_owned)));
        Ok(())
    }

    /// AC-6.3b-3.
    #[tokio::test(start_paused = true)]
    async fn ac_6_3b_3_prerender_skips_other_variables() -> anyhow::Result<()> {
        let (speech, tts) = speech();
        prerender(&speech).await?;
        let texts = tts.texts();
        assert!(
            !texts.iter().any(|text| text.contains("degrees")),
            "an entity template was synthesized: {texts:?}"
        );
        Ok(())
    }

    /// AC-6.3b-5.
    #[tokio::test(start_paused = true)]
    async fn ac_6_3b_5_miss_synthesizes_without_store() -> anyhow::Result<()> {
        let (speech, tts) = speech();
        prerender(&speech).await?;
        let (size, calls) = (speech.cache.len(), tts.texts().len());
        speech.speak("It is 21 degrees in the office.").await?;
        assert_eq!(tts.texts()[calls..], ["It is 21 degrees in the office."]);
        assert_eq!(speech.cache.len(), size, "a miss changed the cache size");
        Ok(())
    }
}
