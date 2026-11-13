//! Minecraft client JVM launch, auto-connect flags, stdout/stderr streaming, and timeout enforcement.

mod config;
mod launch;
mod monitor;

pub use config::{
    build_connect_args, ensure_default_options_txt, strip_mojang_placeholders,
    supports_quick_play, ClientLaunchConfig,
};
pub use launch::{run_client, ClientRunOutcome};
pub use monitor::rule_supports_early_completion;

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use deming_core::{AccountRef, InstanceSpec, ModLoader, ProcessPriority, TestCompletionRule};
    use uuid::Uuid;

    use super::*;
    use crate::absolutize_executable;

    #[test]
    fn build_args_includes_ram_classpath_account_and_connect_flags() {
        let cfg = ClientLaunchConfig {
            run_id: Uuid::new_v4(),
            java_exe: PathBuf::from("java"),
            instance_dir: PathBuf::from("instances/c1"),
            spec: InstanceSpec {
                id: "c1".to_string(),
                mc_version: "1.21.4".to_string(),
                loader: ModLoader::Fabric,
                loader_version: None,
                mods: vec![],
                account: AccountRef::Offline {
                    username: "TesterOne".to_string(),
                },
                ram_mb: Some(2048),
                cpu_cores: Some(2),
                jvm_args: vec!["-XX:+UseG1GC".to_string()],
                game_args: vec!["--width".to_string(), "1280".to_string()],
                test_rule: TestCompletionRule::ExitCodeZero,
            },
            main_class: Some("net.fabricmc.loader.impl.launch.knot.KnotClient".to_string()),
            classpath: vec![PathBuf::from("a.jar"), PathBuf::from("b.jar")],
            natives_dir: Some(PathBuf::from("natives")),
            assets_dir: Some(PathBuf::from("assets")),
            asset_index: Some("19".to_string()),
            access_token: None,
            ram_mb: 2048,
            cpu_cores: Some(2),
            priority: ProcessPriority::Normal,
            connect_server: Some(("127.0.0.1".to_string(), 25577)),
            timeout_secs: 60,
            extra_jvm_args: vec![],
            extra_game_args: vec![],
            on_pid: None,
        };

        let args = cfg.build_args();
        assert!(args.contains(&"-Xmx2048M".to_string()));
        assert!(args.contains(&"-XX:+UseG1GC".to_string()));
        assert!(args.contains(&"--quickPlayMultiplayer".to_string()));
        assert!(args.contains(&"127.0.0.1:25577".to_string()));
        assert!(args.contains(&"--username".to_string()));
        assert!(args.contains(&"TesterOne".to_string()));
    }

    #[test]
    fn strip_mojang_placeholders_removes_unexpanded_tokens_and_pairs() {
        let input: Vec<String> = [
            "-Dos.name=Windows 10",
            "-Djava.library.path=${natives_directory}",
            "-Dminecraft.launcher.brand=${launcher_name}",
            "-DFabricMcEmu= net.minecraft.client.main.Main ",
            "-cp",
            "${classpath}",
            "--username",
            "${auth_player_name}",
            "--version",
            "${version_name}",
            "--gameDir",
            "${game_directory}",
            "--assetsDir",
            "${assets_root}",
            "--assetIndex",
            "${assets_index_name}",
            "--uuid",
            "${auth_uuid}",
            "--accessToken",
            "${auth_access_token}",
            "--clientId",
            "${clientid}",
            "--xuid",
            "${auth_xuid}",
            "--userType",
            "${user_type}",
            "--versionType",
            "${version_type}",
            "--demo",
            "--width",
            "1280",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        let filtered = strip_mojang_placeholders(&input);
        assert_eq!(
            filtered,
            vec![
                "-Dos.name=Windows 10",
                "-DFabricMcEmu= net.minecraft.client.main.Main ",
                "--demo",
                "--width",
                "1280",
            ]
        );
    }

    #[test]
    fn client_build_args_absolutizes_paths_and_emits_single_natives_and_cp() {
        let cfg = ClientLaunchConfig {
            run_id: Uuid::new_v4(),
            java_exe: PathBuf::from(".deming-test/ws/cache/jre/bin/java.exe"),
            instance_dir: PathBuf::from(".deming-test/ws/instances/c1"),
            spec: InstanceSpec {
                id: "c1".to_string(),
                mc_version: "1.20.1".to_string(),
                loader: ModLoader::Fabric,
                loader_version: None,
                mods: vec![],
                account: AccountRef::Offline {
                    username: "TesterOne".to_string(),
                },
                ram_mb: Some(2048),
                cpu_cores: None,
                jvm_args: vec![],
                game_args: vec![],
                test_rule: TestCompletionRule::ExitCodeZero,
            },
            main_class: Some("net.fabricmc.loader.impl.launch.knot.KnotClient".to_string()),
            classpath: vec![
                PathBuf::from(".deming-test/ws/cache/a.jar"),
                PathBuf::from(".deming-test/ws/cache/b.jar"),
            ],
            natives_dir: Some(PathBuf::from(".deming-test/ws/cache/natives")),
            assets_dir: Some(PathBuf::from(".deming-test/ws/cache/assets")),
            asset_index: Some("5".to_string()),
            access_token: None,
            ram_mb: 2048,
            cpu_cores: None,
            priority: ProcessPriority::Normal,
            connect_server: None,
            timeout_secs: 60,
            extra_jvm_args: vec![
                "-XX:+UseG1GC".to_string(),
                "-Djava.library.path=${natives_directory}".to_string(),
                "-cp".to_string(),
                "${classpath}".to_string(),
            ],
            extra_game_args: vec![
                "--username".to_string(),
                "${auth_player_name}".to_string(),
                "--accessToken".to_string(),
                "${auth_access_token}".to_string(),
                "--demo".to_string(),
            ],
            on_pid: None,
        };

        let args = cfg.build_args();

        // Zero unexpanded placeholders remain.
        assert!(
            !args.iter().any(|a| a.contains("${")),
            "found unexpanded placeholder in {args:?}"
        );

        // -Djava.library.path and -cp appear exactly once with absolute paths.
        let natives_flags: Vec<&String> = args
            .iter()
            .filter(|a| a.starts_with("-Djava.library.path="))
            .collect();
        assert_eq!(natives_flags.len(), 1);
        let natives_val = natives_flags[0].strip_prefix("-Djava.library.path=").unwrap();
        assert!(Path::new(natives_val).is_absolute());

        let cp_count = args.iter().filter(|a| *a == "-cp").count();
        assert_eq!(cp_count, 1);
        let cp_idx = args.iter().position(|a| a == "-cp").unwrap();
        let sep = if cfg!(windows) { ';' } else { ':' };
        for entry in args[cp_idx + 1].split(sep) {
            assert!(Path::new(entry).is_absolute(), "expected absolute cp entry: {entry}");
        }

        // --gameDir and --assetsDir are absolute.
        let game_dir_idx = args.iter().position(|a| a == "--gameDir").unwrap();
        assert!(Path::new(&args[game_dir_idx + 1]).is_absolute());
        let assets_dir_idx = args.iter().position(|a| a == "--assetsDir").unwrap();
        assert!(Path::new(&args[assets_dir_idx + 1]).is_absolute());

        // Preserved user/loader flags.
        assert!(args.contains(&"-XX:+UseG1GC".to_string()));
        assert!(args.contains(&"--demo".to_string()));

        // Relative executable with parent is absolutized; bare command is left intact.
        assert!(absolutize_executable(&cfg.java_exe).is_absolute());
        assert_eq!(absolutize_executable(Path::new("java")), PathBuf::from("java"));
    }

    #[test]
    fn ensure_default_options_txt_skips_onboarding_only_when_missing() {
        let dir = std::env::temp_dir().join(format!("deming-opts-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        ensure_default_options_txt(&dir).unwrap();
        let options_path = dir.join("options.txt");
        assert_eq!(
            std::fs::read_to_string(&options_path).unwrap(),
            "onboardAccessibility:false\npauseOnLostFocus:false\nrawMouseInput:false\n"
        );

        // Existing options.txt must be preserved untouched.
        std::fs::write(&options_path, "lang:en_us\nonboardAccessibility:true\n").unwrap();
        ensure_default_options_txt(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(&options_path).unwrap(),
            "lang:en_us\nonboardAccessibility:true\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rule_supports_early_completion_recognizes_log_rules_only() {
        // Pure LogPattern is eligible for early completion.
        let log_rule = TestCompletionRule::LogPattern {
            success_regex: "joined the game".to_string(),
            failure_regex: Some("FATAL|Exception".to_string()),
        };
        let (succ, fail) = rule_supports_early_completion(&log_rule).expect("LogPattern eligible");
        assert_eq!(succ.len(), 1);
        assert_eq!(fail.len(), 1);

        // Composite of LogPatterns is eligible and collects all sub-regexes.
        let comp_log = TestCompletionRule::Composite(vec![
            log_rule.clone(),
            TestCompletionRule::LogPattern {
                success_regex: "Sound engine started".to_string(),
                failure_regex: None,
            },
        ]);
        let (succ, fail) = rule_supports_early_completion(&comp_log).expect("Composite log eligible");
        assert_eq!(succ.len(), 2);
        assert_eq!(fail.len(), 1);

        // ExitCodeZero, GameTestReport, and Composite containing them are NOT eligible.
        assert!(rule_supports_early_completion(&TestCompletionRule::ExitCodeZero).is_none());
        assert!(rule_supports_early_completion(&TestCompletionRule::GameTestReport {
            report_path_glob: "*.xml".to_string()
        })
        .is_none());
        assert!(rule_supports_early_completion(&TestCompletionRule::Composite(vec![
            log_rule,
            TestCompletionRule::ExitCodeZero,
        ]))
        .is_none());
        assert!(rule_supports_early_completion(&TestCompletionRule::Composite(vec![])).is_none());
    }
}
