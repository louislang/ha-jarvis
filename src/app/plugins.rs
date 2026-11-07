//! The plugin registry: lookup by tool name, result cache and deadline.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::time::Duration;

use anyhow::Context;
use jiff::Timestamp;
use serde_json::Value;

use crate::PluginsConfig;
use crate::domain::decide::PluginCall;
use crate::domain::state::HomeConfig;
use crate::ports::Clock;
use crate::ports::Plugin;
use crate::ports::Tool;

/// The plugins by tool name, with the last result of each call.
pub struct Plugins {
    /// The plugins by tool name.
    by_name: BTreeMap<String, Arc<dyn Plugin>>,
    /// The cache time and the deadline.
    config: PluginsConfig,
    /// The time of each result.
    clock: Arc<dyn Clock>,
    /// The time and the result of the last fetch, by plugin name and arguments text.
    results: Mutex<HashMap<(String, String), (Timestamp, Value)>>,
}

impl Plugins {
    /// The registry of `plugins` with the cache time and the deadline of `config` and the time
    /// of `clock`.
    #[must_use]
    pub fn new(
        config: PluginsConfig,
        clock: Arc<dyn Clock>,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> Self {
        let named = plugins
            .into_iter()
            .map(|plugin| (plugin.tool().name, plugin));
        Self {
            by_name: named.collect(),
            config,
            clock,
            results: Mutex::default(),
        }
    }

    /// The tools of all plugins.
    #[must_use]
    pub fn tools(&self) -> Vec<Tool> {
        self.by_name.values().map(|plugin| plugin.tool()).collect()
    }

    /// The result of `call` in the house with the HA configuration `home`: the cached result
    /// when it is less than `plugins.cache_s` old, else a new fetch.
    ///
    /// # Errors
    ///
    /// No plugin has the name of `call`, the plugin fails, or it gives no result within
    /// `plugins.timeout_ms`.
    pub async fn fetch(&self, call: &PluginCall, home: &HomeConfig) -> anyhow::Result<Value> {
        let Some(plugin) = self.by_name.get(&call.plugin) else {
            anyhow::bail!("no plugin has the tool {}", call.plugin);
        };
        let key = (call.plugin.clone(), serde_json::to_string(&call.arguments)?);
        let now = self.clock.now();
        if let Some(result) = self.cached(&key, now) {
            return Ok(result);
        }
        let deadline = Duration::from_millis(self.config.timeout_ms);
        let result = tokio::time::timeout(deadline, plugin.fetch(&call.arguments, home))
            .await
            .context("no plugin result within plugins.timeout_ms")??;
        self.results().insert(key, (now, result.clone()));
        Ok(result)
    }

    /// The result of `key` when it is less than `plugins.cache_s` old at `now`.
    fn cached(&self, key: &(String, String), now: Timestamp) -> Option<Value> {
        let results = self.results();
        let (fetched, result) = results.get(key)?;
        let age = u64::try_from(now.duration_since(*fetched).as_secs()).ok()?;
        (age < self.config.cache_s).then(|| result.clone())
    }

    /// The cached results.
    fn results(&self) -> MutexGuard<'_, HashMap<(String, String), (Timestamp, Value)>> {
        self.results.lock().expect("no lock holder panics")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use jiff::SignedDuration;
    use serde_json::json;
    use tokio::time::error::Elapsed;

    use super::*;
    use crate::Config;
    use crate::testing::fakes;
    use crate::testing::fakes::FakePlugin;
    use crate::testing::fakes::SetClock;

    /// The tool name of the test plugin.
    const NAME: &str = "get_test";

    /// The registry of `plugins` with the `[plugins]` of `jarvis.toml` and `clock`.
    fn registry(clock: Arc<SetClock>, plugins: Vec<Arc<dyn Plugin>>) -> Plugins {
        let config = Config::parse(include_str!("../../jarvis.toml")).expect("it is valid");
        Plugins::new(config.plugins, clock, plugins)
    }

    /// A clock at the Unix epoch.
    fn clock() -> Arc<SetClock> {
        Arc::new(SetClock(Mutex::new(Timestamp::UNIX_EPOCH)))
    }

    /// A call of `plugin` with the argument `when`.
    fn call(plugin: &str, when: &str) -> PluginCall {
        let arguments = json!({ "when": when });
        PluginCall {
            plugin: plugin.to_owned(),
            arguments: arguments.as_object().expect("an object").clone(),
        }
    }

    /// The HA configuration of the reference house.
    fn home() -> HomeConfig {
        fakes::house().config
    }

    /// The test plugin with `results` scripted, and the registry of it at `clock`.
    fn scripted(
        clock: Arc<SetClock>,
        results: Vec<anyhow::Result<Value>>,
    ) -> (Arc<FakePlugin>, Plugins) {
        let plugin = Arc::new(FakePlugin::new(NAME));
        for result in results {
            plugin.script(result);
        }
        let registry = registry(clock, vec![Arc::clone(&plugin) as Arc<dyn Plugin>]);
        (plugin, registry)
    }

    /// AC-17.1-1.
    #[tokio::test(start_paused = true)]
    async fn ac_17_1_1_repeat_within_cache_time_fetches_once() -> anyhow::Result<()> {
        let clock = clock();
        let (plugin, registry) = scripted(Arc::clone(&clock), vec![Ok(json!({ "high": 72 }))]);
        let first = registry.fetch(&call(NAME, "today"), &home()).await?;
        *clock.0.lock().expect("no lock holder panics") += SignedDuration::from_secs(299);
        let second = registry.fetch(&call(NAME, "today"), &home()).await?;
        assert_eq!(
            plugin.fetches().len(),
            1,
            "the second fetch calls the plugin"
        );
        assert_eq!(first, second, "the cached result differs");
        Ok(())
    }

    /// AC-17.1-2.
    #[tokio::test(start_paused = true)]
    async fn ac_17_1_2_repeat_after_cache_time_fetches_again() -> anyhow::Result<()> {
        let clock = clock();
        let results = vec![Ok(json!(1)), Ok(json!(2))];
        let (plugin, registry) = scripted(Arc::clone(&clock), results);
        registry.fetch(&call(NAME, "today"), &home()).await?;
        *clock.0.lock().expect("no lock holder panics") += SignedDuration::from_secs(300);
        let second = registry.fetch(&call(NAME, "today"), &home()).await?;
        assert_eq!(plugin.fetches().len(), 2, "the second fetch uses the cache");
        assert_eq!(second, json!(2), "the second result is not the new one");
        Ok(())
    }

    /// AC-17.1-3.
    #[tokio::test(start_paused = true)]
    async fn ac_17_1_3_different_arguments_fetch_twice() -> anyhow::Result<()> {
        let (plugin, registry) = scripted(clock(), vec![Ok(json!(1)), Ok(json!(2))]);
        registry.fetch(&call(NAME, "today"), &home()).await?;
        registry.fetch(&call(NAME, "tomorrow"), &home()).await?;
        let fetches = plugin.fetches();
        let arguments: Vec<&Value> = fetches
            .iter()
            .map(|(arguments, _)| &arguments["when"])
            .collect();
        assert_eq!(
            arguments,
            [&json!("today"), &json!("tomorrow")],
            "fetches: {fetches:?}"
        );
        Ok(())
    }

    /// AC-17.1-4.
    #[tokio::test(start_paused = true)]
    async fn ac_17_1_4_error_is_not_cached() -> anyhow::Result<()> {
        let results = vec![Err(anyhow::anyhow!("server down")), Ok(json!(1))];
        let (plugin, registry) = scripted(clock(), results);
        let error = registry.fetch(&call(NAME, "today"), &home()).await;
        let error = error.expect_err("the plugin error is not returned");
        assert_eq!(error.to_string(), "server down", "another error");
        let second = registry.fetch(&call(NAME, "today"), &home()).await?;
        assert_eq!(plugin.fetches().len(), 2, "the error is cached");
        assert_eq!(second, json!(1), "the second result is not the new one");
        Ok(())
    }

    /// AC-17.1-5.
    #[tokio::test(start_paused = true)]
    async fn ac_17_1_5_unknown_plugin_gives_error() {
        let (plugin, registry) = scripted(clock(), vec![Ok(json!(1))]);
        let result = registry.fetch(&call("get_other", "today"), &home()).await;
        assert!(result.is_err(), "a result without a plugin: {result:?}");
        assert!(plugin.fetches().is_empty(), "another plugin fetches");
    }

    /// AC-17.1-6.
    #[tokio::test(start_paused = true)]
    async fn ac_17_1_6_no_result_within_timeout_gives_timeout_error() {
        let (_plugin, registry) = scripted(clock(), Vec::new());
        let start = tokio::time::Instant::now();
        let result = registry.fetch(&call(NAME, "today"), &home()).await;
        let error = result.expect_err("a result without a plugin result");
        assert!(error.is::<Elapsed>(), "not a timeout: {error:#}");
        let waited = tokio::time::Instant::now() - start;
        assert_eq!(
            waited,
            Duration::from_millis(5000),
            "not the timeout_ms of jarvis.toml"
        );
    }

    /// AC-17.1-7.
    #[tokio::test(start_paused = true)]
    async fn ac_17_1_7_tools_of_two_plugins() {
        let plugins: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(FakePlugin::new("get_b")),
            Arc::new(FakePlugin::new("get_a")),
        ];
        let names: BTreeSet<String> = registry(clock(), plugins)
            .tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(
            names,
            BTreeSet::from(["get_a".to_owned(), "get_b".to_owned()])
        );
    }
}
