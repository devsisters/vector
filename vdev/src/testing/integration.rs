use std::{collections::BTreeMap, path::Path, path::PathBuf, process::Command};

use anyhow::{bail, Context, Result};

use super::config::{Environment, IntegrationTestConfig, RustToolchainConfig};
use super::runner::{
    ContainerTestRunner as _, IntegrationTestRunner, TestRunner as _, CONTAINER_TOOL,
};
use super::state::EnvsDir;
use crate::app::{self, CommandExt as _};
use crate::util::exists;

const NETWORK_ENV_VAR: &str = "VECTOR_NETWORK";

#[allow(clippy::dbg_macro)]
fn old_integration_path(integration: &str) -> PathBuf {
    let filename = format!("docker-compose.{integration}.yml");
    [app::path(), "scripts", "integration", &filename]
        .into_iter()
        .collect()
}

pub fn old_exists(integration: &str) -> Result<bool> {
    let path = old_integration_path(integration);
    exists(path)
}

/// Temporary runner setup for old-style integration tests
pub struct OldIntegrationTest {
    compose_path: PathBuf,
}

impl OldIntegrationTest {
    pub fn new(integration: &str) -> Self {
        let compose_path = old_integration_path(integration);
        Self { compose_path }
    }

    pub fn build(&self) -> Result<()> {
        self.compose(&["build"])
    }

    pub fn test(&self) -> Result<()> {
        self.compose(&["run", "--rm", "runner"])
    }

    pub fn stop(&self) -> Result<()> {
        self.compose(&["rm", "--force", "--stop", "-v"])
    }

    fn compose(&self, args: &[&'static str]) -> Result<()> {
        let mut command = CONTAINER_TOOL.clone();
        command.push("-compose");
        let mut command = Command::new(command);
        command.arg("--file");
        command.arg(&self.compose_path);
        command.args(args);
        command.current_dir(app::path());

        let rust_version = RustToolchainConfig::parse()
            .expect("Could not parse `rust-toolchain.toml`")
            .channel;
        command.env("RUST_VERSION", rust_version);

        command.check_run()
    }
}

pub struct IntegrationTest {
    integration: String,
    environment: String,
    test_dir: PathBuf,
    config: IntegrationTestConfig,
    envs_dir: EnvsDir,
    runner: IntegrationTestRunner,
}

impl IntegrationTest {
    pub fn new(integration: impl Into<String>, environment: impl Into<String>) -> Result<Self> {
        let integration = integration.into();
        let environment = environment.into();
        let (test_dir, config) = IntegrationTestConfig::load(&integration)?;
        let envs_dir = EnvsDir::new(&integration);
        let runner = IntegrationTestRunner::new(integration.clone())?;

        Ok(Self {
            integration,
            environment,
            test_dir,
            config,
            envs_dir,
            runner,
        })
    }

    pub fn test(&self, env_vars: &BTreeMap<String, String>, args: &[String]) -> Result<()> {
        let active = self.envs_dir.check_active(&self.environment)?;

        if !active {
            self.start()?;
        }

        self.runner.test(env_vars, args)?;

        if !active {
            self.runner.remove()?;
            self.stop()?;
        }
        Ok(())
    }

    pub fn start(&self) -> Result<()> {
        self.runner.ensure_network()?;

        let environments = self.config.environments();
        let cmd_config = match environments.get(&self.environment) {
            Some(config) => config,
            None => bail!("unknown environment: {}", self.environment),
        };

        if self.envs_dir.check_active(&self.environment)? {
            bail!("environment is already up");
        }

        self.run_compose("Starting", &["up", "--detach"], cmd_config)?;

        self.envs_dir.save(&self.environment, cmd_config)
    }

    pub fn stop(&self) -> Result<()> {
        let Some(state) = self.envs_dir.load()? else {
             bail!("No environment for {} is up.",self.integration);
        };

        self.runner.remove()?;
        self.run_compose(
            "Stopping",
            &["down", "--timeout", "0", "--volumes"],
            &state.config,
        )?;
        self.envs_dir.remove()?;

        Ok(())
    }

    fn run_compose(&self, action: &str, args: &[&'static str], config: &Environment) -> Result<()> {
        let compose_path: PathBuf = [&self.test_dir, Path::new("compose.yaml")].iter().collect();
        let compose_file = dunce::canonicalize(compose_path)
            .context("Could not canonicalize docker compose path")?
            .display()
            .to_string();

        let mut command = CONTAINER_TOOL.clone();
        command.push("-compose");
        let mut command = Command::new(command);
        command.args(["--file", &compose_file]);
        command.args(args);

        command.current_dir(&self.test_dir);

        command.env(NETWORK_ENV_VAR, self.runner.network_name());
        if let Some(env_vars) = &self.config.env {
            command.envs(env_vars);
        }
        // TODO: Export all config variables, not just `version`
        if let Some(version) = config.get("version") {
            let version_env = format!(
                "{}_VERSION",
                self.integration.replace('-', "_").to_uppercase()
            );
            command.env(version_env, version);
        }

        waiting!("{action} environment {}", self.environment);
        command.check_run()
    }
}
