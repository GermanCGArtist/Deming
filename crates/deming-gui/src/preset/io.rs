//! Validation, `TestJobSpec` bidirectional conversion, and JSON preset serialization.

use std::collections::BTreeMap;
use std::path::PathBuf;

use deming_auth::StoredAccount;
use deming_core::{
    AccountRef, InstanceSpec, ModSource, ServerSpec, TestCompletionRule, TestJobSpec,
};

use super::schema::{
    AccountDraft, BuilderState, CompletionRuleDraft, CompletionRuleKind, InstanceDraft,
    ModEntryDraft, ServerDraft,
};

impl ModEntryDraft {
    pub fn to_mod_source(&self) -> Result<ModSource, String> {
        if self.is_modrinth {
            let slug = self.modrinth_slug.trim();
            if slug.is_empty() {
                return Err("Modrinth project ID or slug cannot be empty".to_string());
            }
            let ver = self.modrinth_version_id.trim();
            Ok(ModSource::Modrinth {
                project_id_or_slug: slug.to_string(),
                version_id: if ver.is_empty() {
                    None
                } else {
                    Some(ver.to_string())
                },
            })
        } else {
            let p = self.local_path.trim();
            if p.is_empty() {
                return Err("Local mod path cannot be empty".to_string());
            }
            Ok(ModSource::LocalPath(PathBuf::from(p)))
        }
    }

    pub fn from_mod_source(src: &ModSource) -> Self {
        match src {
            ModSource::LocalPath(p) => Self::local(p.to_string_lossy().to_string()),
            ModSource::Modrinth {
                project_id_or_slug,
                version_id,
            } => Self::modrinth(project_id_or_slug.clone(), version_id.clone()),
        }
    }
}

impl CompletionRuleDraft {
    pub fn to_rule(&self) -> Result<TestCompletionRule, String> {
        match self.kind {
            CompletionRuleKind::ExitCodeZero => Ok(TestCompletionRule::ExitCodeZero),
            CompletionRuleKind::LogPattern => {
                let succ = self.success_regex.trim();
                if succ.is_empty() {
                    return Err("LogPattern success_regex cannot be empty".to_string());
                }
                regex::Regex::new(succ)
                    .map_err(|e| format!("Invalid success_regex '{succ}': {e}"))?;
                let fail = self.failure_regex.trim();
                let failure_regex = if fail.is_empty() {
                    None
                } else {
                    regex::Regex::new(fail)
                        .map_err(|e| format!("Invalid failure_regex '{fail}': {e}"))?;
                    Some(fail.to_string())
                };
                Ok(TestCompletionRule::LogPattern {
                    success_regex: succ.to_string(),
                    failure_regex,
                })
            }
            CompletionRuleKind::GameTestReport => {
                let glob = self.report_path_glob.trim();
                if glob.is_empty() {
                    return Err("GameTestReport report_path_glob cannot be empty".to_string());
                }
                Ok(TestCompletionRule::GameTestReport {
                    report_path_glob: glob.to_string(),
                })
            }
            CompletionRuleKind::Composite => {
                if self.sub_rules.is_empty() {
                    return Err(
                        "Composite completion rule requires at least one sub-rule".to_string()
                    );
                }
                let mut out = Vec::with_capacity(self.sub_rules.len());
                for sub in &self.sub_rules {
                    if sub.kind == CompletionRuleKind::Composite {
                        return Err("Nested Composite rules are simplified to flat sub-rules in the UI; please use non-composite sub-rules".to_string());
                    }
                    out.push(sub.to_rule()?);
                }
                Ok(TestCompletionRule::Composite(out))
            }
        }
    }

    pub fn from_rule(rule: &TestCompletionRule) -> Self {
        match rule {
            TestCompletionRule::ExitCodeZero => Self::exit_code_zero(),
            TestCompletionRule::LogPattern {
                success_regex,
                failure_regex,
            } => Self {
                kind: CompletionRuleKind::LogPattern,
                success_regex: success_regex.clone(),
                failure_regex: failure_regex.clone().unwrap_or_default(),
                ..Default::default()
            },
            TestCompletionRule::GameTestReport { report_path_glob } => Self {
                kind: CompletionRuleKind::GameTestReport,
                report_path_glob: report_path_glob.clone(),
                ..Default::default()
            },
            TestCompletionRule::Composite(subs) => Self {
                kind: CompletionRuleKind::Composite,
                sub_rules: subs.iter().map(Self::from_rule).collect(),
                ..Default::default()
            },
        }
    }
}

impl AccountDraft {
    pub fn from_account_ref(acc: &AccountRef) -> Self {
        match acc {
            AccountRef::Offline { username } => Self {
                is_microsoft: false,
                username: username.clone(),
                uuid: String::new(),
            },
            AccountRef::Microsoft { uuid, username } => Self {
                is_microsoft: true,
                username: username.clone(),
                uuid: uuid.clone(),
            },
        }
    }

    pub fn from_stored(acc: &StoredAccount) -> Self {
        Self::from_account_ref(&acc.to_account_ref())
    }

    pub fn to_account_ref(&self) -> Result<AccountRef, String> {
        let user = self.username.trim();
        if user.is_empty() {
            return Err("Account username cannot be empty".to_string());
        }
        if self.is_microsoft {
            let uuid = self.uuid.trim();
            if uuid.is_empty() {
                return Err("Microsoft account UUID cannot be empty".to_string());
            }
            Ok(AccountRef::Microsoft {
                uuid: uuid.to_string(),
                username: user.to_string(),
            })
        } else {
            Ok(AccountRef::Offline {
                username: user.to_string(),
            })
        }
    }
}

impl InstanceDraft {
    pub fn to_instance_spec(&self) -> Result<InstanceSpec, String> {
        let id = self.id.trim();
        if id.is_empty() {
            return Err("Instance ID cannot be empty".to_string());
        }
        let mc = self.mc_version.trim();
        if mc.is_empty() {
            return Err(format!("Instance '{id}' MC version cannot be empty"));
        }

        let mut mods = Vec::with_capacity(self.mods.len());
        for m in &self.mods {
            mods.push(m.to_mod_source()?);
        }

        let loader_ver = self.loader_version.trim();
        let loader_version = if loader_ver.is_empty() {
            None
        } else {
            Some(loader_ver.to_string())
        };

        Ok(InstanceSpec {
            id: id.to_string(),
            mc_version: mc.to_string(),
            loader: self.loader,
            loader_version,
            mods,
            account: self.account.to_account_ref()?,
            ram_mb: if self.use_custom_ram {
                Some(self.ram_mb.max(256))
            } else {
                None
            },
            cpu_cores: if self.use_custom_cpu {
                Some(self.cpu_cores.max(1))
            } else {
                None
            },
            jvm_args: split_whitespace_args(&self.jvm_args_text),
            game_args: split_whitespace_args(&self.game_args_text),
            test_rule: self.test_rule.to_rule()?,
        })
    }

    pub fn from_instance_spec(spec: &InstanceSpec) -> Self {
        Self {
            id: spec.id.clone(),
            mc_version: spec.mc_version.clone(),
            loader: spec.loader,
            loader_version: spec.loader_version.clone().unwrap_or_default(),
            mods: spec.mods.iter().map(ModEntryDraft::from_mod_source).collect(),
            account: AccountDraft::from_account_ref(&spec.account),
            use_custom_ram: spec.ram_mb.is_some(),
            ram_mb: spec.ram_mb.unwrap_or(2048),
            use_custom_cpu: spec.cpu_cores.is_some(),
            cpu_cores: spec.cpu_cores.unwrap_or(2),
            jvm_args_text: spec.jvm_args.join(" "),
            game_args_text: spec.game_args.join(" "),
            test_rule: CompletionRuleDraft::from_rule(&spec.test_rule),
        }
    }
}

impl ServerDraft {
    pub fn to_server_spec(&self) -> Result<Option<ServerSpec>, String> {
        if !self.enabled {
            return Ok(None);
        }
        let mc = self.mc_version.trim();
        if mc.is_empty() {
            return Err("Server MC version cannot be empty".to_string());
        }
        let ready = self.ready_pattern.trim();
        if ready.is_empty() {
            return Err("Server ready_pattern cannot be empty".to_string());
        }
        regex::Regex::new(ready)
            .map_err(|e| format!("Invalid server ready_pattern '{ready}': {e}"))?;

        let mut mods = Vec::with_capacity(self.mods.len());
        for m in &self.mods {
            mods.push(m.to_mod_source()?);
        }

        let server_properties = parse_properties_lines(&self.properties_text)?;
        let loader_ver = self.loader_version.trim();

        Ok(Some(ServerSpec {
            mc_version: mc.to_string(),
            loader: self.loader,
            loader_version: if loader_ver.is_empty() {
                None
            } else {
                Some(loader_ver.to_string())
            },
            mods,
            online_mode: self.online_mode,
            port: if self.auto_port {
                None
            } else {
                Some(self.port.max(1))
            },
            server_properties,
            ready_pattern: ready.to_string(),
            auto_connect_clients: self.auto_connect_clients,
            ram_mb: if self.use_custom_ram {
                Some(self.ram_mb.max(512))
            } else {
                None
            },
        }))
    }

    pub fn from_server_spec(spec: Option<&ServerSpec>) -> Self {
        match spec {
            None => Self::default(),
            Some(s) => {
                let props = s
                    .server_properties
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                Self {
                    enabled: true,
                    mc_version: s.mc_version.clone(),
                    loader: s.loader,
                    loader_version: s.loader_version.clone().unwrap_or_default(),
                    mods: s.mods.iter().map(ModEntryDraft::from_mod_source).collect(),
                    online_mode: s.online_mode,
                    auto_port: s.port.is_none(),
                    port: s.port.unwrap_or(25565),
                    ready_pattern: s.ready_pattern.clone(),
                    auto_connect_clients: s.auto_connect_clients,
                    use_custom_ram: s.ram_mb.is_some(),
                    ram_mb: s.ram_mb.unwrap_or(2048),
                    properties_text: props,
                }
            }
        }
    }
}

impl BuilderState {
    /// Validates and converts the UI state into a [`TestJobSpec`].
    pub fn to_test_job_spec(&self) -> Result<TestJobSpec, String> {
        let name = self.job_name.trim();
        if name.is_empty() {
            return Err("Job name cannot be empty".to_string());
        }
        if self.instances.is_empty() {
            return Err("At least one client instance is required".to_string());
        }

        let mut seen_ids = std::collections::HashSet::new();
        let mut instances = Vec::with_capacity(self.instances.len());
        for inst in &self.instances {
            let spec = inst.to_instance_spec()?;
            if !seen_ids.insert(spec.id.clone()) {
                return Err(format!("Duplicate instance ID '{}'", spec.id));
            }
            instances.push(spec);
        }

        let server = self.server.to_server_spec()?;
        Ok(TestJobSpec {
            name: name.to_string(),
            instances,
            server,
            timeout_secs: self.timeout_secs,
        })
    }

    /// Populates a [`BuilderState`] from an existing [`TestJobSpec`].
    pub fn from_test_job_spec(spec: &TestJobSpec) -> Self {
        Self {
            job_name: spec.name.clone(),
            timeout_secs: spec.timeout_secs,
            instances: spec
                .instances
                .iter()
                .map(InstanceDraft::from_instance_spec)
                .collect(),
            server: ServerDraft::from_server_spec(spec.server.as_ref()),
        }
    }

    /// Serializes this builder state as a [`TestJobSpec`] JSON preset.
    pub fn to_preset_json(&self) -> Result<String, String> {
        let spec = self.to_test_job_spec()?;
        serde_json::to_string_pretty(&spec).map_err(|e| format!("Failed to serialize preset: {e}"))
    }

    /// Parses a [`TestJobSpec`] JSON preset into a [`BuilderState`].
    pub fn from_preset_json(raw: &str) -> Result<Self, String> {
        let spec: TestJobSpec =
            serde_json::from_str(raw).map_err(|e| format!("Invalid preset JSON: {e}"))?;
        Ok(Self::from_test_job_spec(&spec))
    }
}

fn split_whitespace_args(raw: &str) -> Vec<String> {
    raw.split_whitespace().map(str::to_string).collect()
}

fn parse_properties_lines(raw: &str) -> Result<BTreeMap<String, String>, String> {
    let mut map = BTreeMap::new();
    for (idx, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((k, v)) = trimmed.split_once('=') else {
            return Err(format!(
                "Invalid server.properties line {}: expected key=value",
                idx + 1
            ));
        };
        let key = k.trim();
        if key.is_empty() {
            return Err(format!(
                "Empty property key on server.properties line {}",
                idx + 1
            ));
        }
        map.insert(key.to_string(), v.trim().to_string());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use deming_core::ModLoader;

    #[test]
    fn builder_state_round_trips_full_test_job_spec() {
        let mut props = BTreeMap::new();
        props.insert("difficulty".to_string(), "peaceful".to_string());
        props.insert("gamemode".to_string(), "creative".to_string());

        let original = TestJobSpec {
            name: "multiplayer-fabric-check".to_string(),
            instances: vec![
                InstanceSpec {
                    id: "client-1".to_string(),
                    mc_version: "1.21.4".to_string(),
                    loader: ModLoader::Fabric,
                    loader_version: Some("0.16.10".to_string()),
                    mods: vec![
                        ModSource::LocalPath(PathBuf::from("mods/my-mod.jar")),
                        ModSource::Modrinth {
                            project_id_or_slug: "fabric-api".to_string(),
                            version_id: Some("0.115.0".to_string()),
                        },
                    ],
                    account: AccountRef::Offline {
                        username: "DevOne".to_string(),
                    },
                    ram_mb: Some(2048),
                    cpu_cores: Some(2),
                    jvm_args: vec!["-XX:+UseG1GC".to_string()],
                    game_args: vec!["--width".to_string(), "1280".to_string()],
                    test_rule: TestCompletionRule::Composite(vec![
                        TestCompletionRule::ExitCodeZero,
                        TestCompletionRule::LogPattern {
                            success_regex: "All tests passed".to_string(),
                            failure_regex: Some("FATAL".to_string()),
                        },
                        TestCompletionRule::GameTestReport {
                            report_path_glob: "reports/*.xml".to_string(),
                        },
                    ]),
                },
                InstanceSpec {
                    id: "client-2".to_string(),
                    mc_version: "1.21.4".to_string(),
                    loader: ModLoader::NeoForge,
                    loader_version: None,
                    mods: vec![],
                    account: AccountRef::Microsoft {
                        uuid: "12345678-1234-1234-1234-1234567890ab".to_string(),
                        username: "RealPlayer".to_string(),
                    },
                    ram_mb: None,
                    cpu_cores: None,
                    jvm_args: vec![],
                    game_args: vec![],
                    test_rule: TestCompletionRule::ExitCodeZero,
                },
            ],
            server: Some(ServerSpec {
                mc_version: "1.21.4".to_string(),
                loader: ModLoader::Fabric,
                loader_version: Some("0.16.10".to_string()),
                mods: vec![ModSource::Modrinth {
                    project_id_or_slug: "fabric-api".to_string(),
                    version_id: None,
                }],
                online_mode: false,
                port: Some(25565),
                server_properties: props,
                ready_pattern: r"Done \([0-9.]+s\)! For help, type".to_string(),
                auto_connect_clients: true,
                ram_mb: Some(4096),
            }),
            timeout_secs: 240,
        };

        let draft = BuilderState::from_test_job_spec(&original);
        let json = draft.to_preset_json().expect("serialize preset");
        let reloaded = BuilderState::from_preset_json(&json).expect("reload preset");
        let converted = reloaded.to_test_job_spec().expect("convert back");

        assert_eq!(converted, original);
    }

    #[test]
    fn builder_validation_rejects_duplicate_ids_and_bad_regex() {
        let mut state = BuilderState::default();
        state.instances.push(InstanceDraft::new_with_index(1));
        let err = state.to_test_job_spec().unwrap_err();
        assert!(err.contains("Duplicate instance ID"));

        let mut single = BuilderState::default();
        single.instances[0].test_rule.kind = CompletionRuleKind::LogPattern;
        single.instances[0].test_rule.success_regex = "[unclosed(".to_string();
        assert!(single.to_test_job_spec().is_err());
    }

    #[test]
    fn builder_allows_auto_server_port_and_disabled_server() {
        let mut state = BuilderState::default();
        state.server.enabled = true;
        state.server.auto_port = true;
        state.server.properties_text = "# comment\npvp=false\nview-distance = 8\n".to_string();

        let spec = state.to_test_job_spec().expect("valid spec");
        let srv = spec.server.expect("server present");
        assert_eq!(srv.port, None);
        assert_eq!(srv.server_properties.get("pvp"), Some(&"false".to_string()));
        assert_eq!(
            srv.server_properties.get("view-distance"),
            Some(&"8".to_string())
        );

        state.server.enabled = false;
        let spec_no_srv = state.to_test_job_spec().expect("valid spec without server");
        assert!(spec_no_srv.server.is_none());
    }
}
