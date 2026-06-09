//! T1 contract tests for the snapdir-ssh-store scaffold: argument grammar
//! (mock-store parity, divergences documented in `src/args.rs`), URL parsing,
//! env-family config + the un-weakenable security-floor flag ordering,
//! `ssh -V` floor parsing, quoting/heredoc helpers, and the emitted script
//! skeleton's textual invariants.

use std::ffi::OsString;
use std::sync::Mutex;

use snapdir_ssh_store::args::{parse, Invocation, Subcommand};
use snapdir_ssh_store::config::{
    Config, DEFAULT_CONNECT_TIMEOUT, DEFAULT_CONTROL_PERSIST, DEFAULT_JOBS, DEFAULT_UMASK,
    FLOOR_CIPHERS, FLOOR_HOST_KEY_ALGORITHMS, FLOOR_KEX_ALGORITHMS,
};
use snapdir_ssh_store::script::{
    heredoc, remote_manifest_path, remote_object_path, sftp_quote, sh_quote, skeleton,
};
use snapdir_ssh_store::url::SshUrl;
use snapdir_ssh_store::version::{check_openssh_floor, parse_openssh_version, MIN_OPENSSH};
use snapdir_ssh_store::{run_with, Engine};

/// Serializes every test that touches the process environment (`from_env`
/// and the `run_with` dispatcher, which calls it): std env is process-global,
/// same pattern as `RATELIMIT_ENV_LOCK` in snapdir-cli.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn os_args(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// An env lookup over a fixed table (the pure injection seam — no process
/// env, no lock needed).
fn lookup_from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    }
}

fn no_env(_: &str) -> Option<String> {
    None
}

fn default_config() -> Config {
    Config::from_lookup(Engine::Ssh, no_env).unwrap()
}

fn parse_url(input: &str) -> SshUrl {
    SshUrl::parse(Engine::Ssh, input).unwrap()
}

fn url_err(input: &str) -> String {
    SshUrl::parse(Engine::Ssh, input).unwrap_err().to_string()
}

// ---------------------------------------------------------------------------
// args: mock grammar parity (`--k v`, `--k=v`, `--version`) + rejections
// ---------------------------------------------------------------------------

#[test]
fn args_space_separated_form_parses() {
    let parsed = parse(os_args(&[
        "get-push-command",
        "--id",
        "abc123",
        "--store",
        "ssh://host/srv/snap",
        "--staging-dir",
        "/tmp/staging",
    ]))
    .unwrap();
    let Invocation::Command(cmd) = parsed else {
        panic!("expected a command invocation");
    };
    assert_eq!(cmd.subcommand, Subcommand::GetPushCommand);
    assert_eq!(cmd.id, "abc123");
    assert_eq!(cmd.store, "ssh://host/srv/snap");
    assert_eq!(cmd.staging_dir.as_deref(), Some("/tmp/staging"));
    assert_eq!(cmd.cache_dir, None);
}

#[test]
fn args_equals_form_parses() {
    let parsed = parse(os_args(&[
        "get-fetch-files-command",
        "--id=abc123",
        "--store=sftp://host/srv/snap",
        "--cache-dir=/tmp/cache",
    ]))
    .unwrap();
    let Invocation::Command(cmd) = parsed else {
        panic!("expected a command invocation");
    };
    assert_eq!(cmd.subcommand, Subcommand::GetFetchFilesCommand);
    assert_eq!(cmd.cache_dir.as_deref(), Some("/tmp/cache"));
}

#[test]
fn args_mixed_forms_and_subcommand_position_independent() {
    let parsed = parse(os_args(&[
        "--id=abc",
        "--store",
        "ssh://h/base",
        "get-manifest-command",
    ]))
    .unwrap();
    let Invocation::Command(cmd) = parsed else {
        panic!("expected a command invocation");
    };
    assert_eq!(cmd.subcommand, Subcommand::GetManifestCommand);
    assert_eq!(cmd.id, "abc");
}

#[test]
fn args_version_tokens_short_circuit() {
    for version_arg in ["-v", "--version", "version"] {
        let parsed = parse(os_args(&["get-push-command", "--id", "x", version_arg])).unwrap();
        assert_eq!(parsed, Invocation::Version, "token {version_arg}");
    }
}

#[test]
fn args_rejections_are_clear() {
    // (args, expected error fragment) — divergence from the mock's silent
    // skip/last-wins is deliberate; see src/args.rs.
    let cases: &[(&[&str], &str)] = &[
        (
            &[
                "get-manifest-command",
                "--id",
                "x",
                "--store",
                "s",
                "--bogus",
                "v",
            ],
            "unknown option '--bogus'",
        ),
        (
            &["get-manifest-command", "stray", "--id", "x", "--store", "s"],
            "unexpected argument 'stray'",
        ),
        (
            &["get-manifest-command", "--id"],
            "missing value for '--id'",
        ),
        (
            &[
                "get-manifest-command",
                "--id",
                "a",
                "--id",
                "b",
                "--store",
                "s",
            ],
            "given more than once",
        ),
        (
            &[
                "get-manifest-command",
                "get-push-command",
                "--id",
                "x",
                "--store",
                "s",
            ],
            "multiple subcommands",
        ),
        (&["--id", "x", "--store", "s"], "missing subcommand"),
        (
            &["get-manifest-command", "--store", "s"],
            "missing required option --id",
        ),
        (
            &["get-manifest-command", "--id", "x"],
            "missing required option --store",
        ),
        (
            &["get-push-command", "--id", "x", "--store", "s"],
            "missing required option --staging-dir",
        ),
        (
            &["get-fetch-files-command", "--id", "x", "--store", "s"],
            "missing required option --cache-dir",
        ),
    ];
    for (args, fragment) in cases {
        let err = parse(os_args(args)).unwrap_err().to_string();
        assert!(
            err.contains(fragment),
            "args {args:?}: expected {fragment:?} in {err:?}"
        );
    }
}

#[test]
fn args_manifest_command_needs_no_dirs() {
    let parsed = parse(os_args(&[
        "get-manifest-command",
        "--id",
        "x",
        "--store",
        "s",
    ]))
    .unwrap();
    assert!(matches!(parsed, Invocation::Command(_)));
}

// ---------------------------------------------------------------------------
// url: grammar table (incl. IPv6, password rejection, hostile chars)
// ---------------------------------------------------------------------------

#[test]
fn url_minimal_parses() {
    let url = parse_url("ssh://example.com/srv/snapdir");
    assert_eq!(url.user, None);
    assert_eq!(url.host, "example.com");
    assert_eq!(url.port, None);
    assert_eq!(url.base, "/srv/snapdir");
    assert_eq!(url.host_arg(), "example.com");
}

#[test]
fn url_user_and_port_parse() {
    let url = parse_url("ssh://deploy_1.bot@snap-host.example.com:2222/srv/snap");
    assert_eq!(url.user.as_deref(), Some("deploy_1.bot"));
    assert_eq!(url.host, "snap-host.example.com");
    assert_eq!(url.port, Some(2222));
    assert_eq!(url.base, "/srv/snap");
}

#[test]
fn url_sftp_scheme_is_distinct() {
    let url = SshUrl::parse(Engine::Sftp, "sftp://example.com/srv/snap").unwrap();
    assert_eq!(url.host, "example.com");

    let err = SshUrl::parse(Engine::Sftp, "ssh://example.com/srv/snap")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("sftp://"),
        "should name the expected scheme: {err}"
    );
    let err = url_err("sftp://example.com/srv/snap");
    assert!(
        err.contains("ssh://"),
        "should name the expected scheme: {err}"
    );
}

#[test]
fn url_bracketed_ipv6_parses_with_and_without_port() {
    let url = parse_url("ssh://backup@[2001:db8::1]:2200/srv/snap");
    assert_eq!(url.user.as_deref(), Some("backup"));
    assert_eq!(url.host, "2001:db8::1");
    assert_eq!(url.port, Some(2200));
    assert_eq!(
        url.host_arg(),
        "[2001:db8::1]",
        "brackets restored for argv"
    );

    let url = parse_url("ssh://[::1]/srv/snap");
    assert_eq!(url.host, "::1");
    assert_eq!(url.port, None);
}

#[test]
fn url_password_rejected_naming_keys_and_agent() {
    let err = url_err("ssh://user:hunter2@example.com/srv/snap");
    assert!(err.contains("password"), "should name the problem: {err}");
    assert!(
        err.contains("IdentityFile") && err.contains("agent"),
        "should point at keys/agent: {err}"
    );
}

#[test]
fn url_base_path_rules() {
    // Bare root and missing base are rejected.
    assert!(url_err("ssh://example.com/").contains("base path"));
    assert!(url_err("ssh://example.com").contains("missing absolute base path"));
    // Trailing slashes trim; inner bytes stay literal (no percent-decoding).
    assert_eq!(parse_url("ssh://h/srv/snap/").base, "/srv/snap");
    assert_eq!(parse_url("ssh://h/a%20b/c d").base, "/a%20b/c d");
    // Control characters (and NUL) are rejected.
    assert!(url_err("ssh://h/srv/\tsnap").contains("control"));
    assert!(url_err("ssh://h/srv/\u{0}snap").contains("control"));
}

#[test]
fn url_hostile_users_and_hosts_rejected() {
    for (input, what) in [
        ("ssh:///srv/snap", "invalid host"),
        ("ssh://@example.com/srv", "invalid user"),
        ("ssh://-oProxyCommand=evil@example.com/srv", "invalid user"),
        ("ssh://user@/srv", "invalid host"),
        ("ssh://-evil.example.com/srv", "invalid host"),
        ("ssh://host;rm -rf ~/srv", "invalid host"),
        ("ssh://host$(x)/srv", "invalid host"),
        ("ssh://[zz8::1]/srv", "invalid IPv6 host"),
        ("ssh://[2001:db8::1/srv", "unterminated '['"),
        ("ssh://[::1]junk/srv", "unexpected text after ']'"),
    ] {
        let err = url_err(input);
        assert!(err.contains(what), "{input}: expected {what:?} in {err:?}");
    }
}

#[test]
fn url_port_bounds() {
    assert!(url_err("ssh://h:0/srv").contains("invalid port"));
    assert!(url_err("ssh://h:70000/srv").contains("invalid port"));
    assert!(url_err("ssh://h:2a2/srv").contains("invalid port"));
    assert_eq!(parse_url("ssh://h:65535/srv").port, Some(65535));
    assert_eq!(parse_url("ssh://h:1/srv").port, Some(1));
}

// ---------------------------------------------------------------------------
// config: env family, fallbacks, validation
// ---------------------------------------------------------------------------

#[test]
fn config_defaults() {
    let cfg = default_config();
    assert_eq!(cfg.identity_file, None);
    assert_eq!(cfg.port, None);
    assert_eq!(cfg.known_hosts, None);
    assert_eq!(cfg.connect_timeout, DEFAULT_CONNECT_TIMEOUT);
    assert_eq!(cfg.connect_timeout, 10);
    assert_eq!(cfg.jobs, DEFAULT_JOBS);
    assert_eq!(cfg.jobs, 4);
    assert_eq!(cfg.control_persist, DEFAULT_CONTROL_PERSIST);
    assert_eq!(cfg.control_persist, 60);
    assert_eq!(cfg.umask, DEFAULT_UMASK);
    assert_eq!(cfg.umask, "077");
    assert!(cfg.extra_opts.is_empty());
}

#[test]
fn config_engine_prefixes_are_disjoint() {
    let pairs = [
        ("SNAPDIR_SFTP_STORE_CONNECT_TIMEOUT", "20"),
        ("SNAPDIR_SFTP_STORE_IDENTITY_FILE", "/keys/sftp_ed25519"),
    ];
    let sftp = Config::from_lookup(Engine::Sftp, lookup_from(&pairs)).unwrap();
    assert_eq!(sftp.connect_timeout, 20);
    assert_eq!(sftp.identity_file.as_deref(), Some("/keys/sftp_ed25519"));

    // The ssh engine must not see the sftp family.
    let ssh = Config::from_lookup(Engine::Ssh, lookup_from(&pairs)).unwrap();
    assert_eq!(ssh.connect_timeout, DEFAULT_CONNECT_TIMEOUT);
    assert_eq!(ssh.identity_file, None);
}

#[test]
fn config_jobs_fallback_chain() {
    let all = [
        ("SNAPDIR_SSH_STORE_JOBS", "9"),
        ("SNAPDIR_JOBS", "7"),
        ("SNAPDIR_MAX_JOBS", "5"),
    ];
    let cfg = Config::from_lookup(Engine::Ssh, lookup_from(&all)).unwrap();
    assert_eq!(cfg.jobs, 9, "engine-family JOBS wins");

    let global = [("SNAPDIR_JOBS", "7"), ("SNAPDIR_MAX_JOBS", "5")];
    let cfg = Config::from_lookup(Engine::Ssh, lookup_from(&global)).unwrap();
    assert_eq!(cfg.jobs, 7, "SNAPDIR_JOBS beats SNAPDIR_MAX_JOBS");

    let max_only = [("SNAPDIR_MAX_JOBS", "5")];
    let cfg = Config::from_lookup(Engine::Ssh, lookup_from(&max_only)).unwrap();
    assert_eq!(cfg.jobs, 5, "SNAPDIR_MAX_JOBS is the last fallback");
}

#[test]
fn config_validation_failures_name_the_variable() {
    for (name, value, fragment) in [
        ("SNAPDIR_SSH_STORE_PORT", "0", "invalid port"),
        ("SNAPDIR_SSH_STORE_PORT", "junk", "invalid port"),
        ("SNAPDIR_SSH_STORE_CONNECT_TIMEOUT", "0", "positive integer"),
        (
            "SNAPDIR_SSH_STORE_CONNECT_TIMEOUT",
            "-2",
            "positive integer",
        ),
        ("SNAPDIR_SSH_STORE_JOBS", "many", "positive integer"),
        ("SNAPDIR_SSH_STORE_UMASK", "789", "octal"),
        ("SNAPDIR_SSH_STORE_UMASK", "07777", "octal"),
        ("SNAPDIR_SSH_STORE_UMASK", "rwx", "octal"),
    ] {
        let pairs = [(name, value)];
        let err = Config::from_lookup(Engine::Ssh, lookup_from(&pairs))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(name),
            "{name}={value}: missing var name in {err:?}"
        );
        assert!(
            err.contains(fragment),
            "{name}={value}: expected {fragment:?} in {err:?}"
        );
    }
}

#[test]
fn config_extra_opts_split_and_validated() {
    let pairs = [(
        "SNAPDIR_SSH_STORE_EXTRA_OPTS",
        "ServerAliveInterval=30  ProxyJump=bastion.example.com Ciphers=^aes256-gcm@openssh.com",
    )];
    let cfg = Config::from_lookup(Engine::Ssh, lookup_from(&pairs)).unwrap();
    assert_eq!(
        cfg.extra_opts,
        vec![
            "ServerAliveInterval=30",
            "ProxyJump=bastion.example.com",
            "Ciphers=^aes256-gcm@openssh.com",
        ]
    );

    for bad in [
        "NoEqualsSign",
        "Key=",
        "1Key=value",
        "Key=va;lue",
        "Key=$(evil)",
        "Key=va`lue",
        "Key='quoted'",
        "Key=a|b",
        "Key=a>b",
        "Key=a*",
    ] {
        let pairs = [("SNAPDIR_SSH_STORE_EXTRA_OPTS", bad)];
        let err = Config::from_lookup(Engine::Ssh, lookup_from(&pairs)).unwrap_err();
        assert!(
            err.to_string().contains("invalid token"),
            "{bad:?} should be rejected: {err}"
        );
    }
}

#[test]
fn config_from_env_reads_the_process_environment() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("SNAPDIR_SSH_STORE_CONNECT_TIMEOUT", "33");
    std::env::set_var("SNAPDIR_SSH_STORE_EXTRA_OPTS", "Compression=yes");
    let cfg = Config::from_env(Engine::Ssh);
    std::env::remove_var("SNAPDIR_SSH_STORE_CONNECT_TIMEOUT");
    std::env::remove_var("SNAPDIR_SSH_STORE_EXTRA_OPTS");
    let cfg = cfg.unwrap();
    assert_eq!(cfg.connect_timeout, 33);
    assert_eq!(cfg.extra_opts, vec!["Compression=yes"]);
}

// ---------------------------------------------------------------------------
// security floor: ordered flag builder
// ---------------------------------------------------------------------------

/// The floor tokens, in their pinned order.
fn floor_tokens(connect_timeout: u32) -> Vec<String> {
    vec![
        "BatchMode=yes".to_owned(),
        "StrictHostKeyChecking=yes".to_owned(),
        "PasswordAuthentication=no".to_owned(),
        "KbdInteractiveAuthentication=no".to_owned(),
        "ClearAllForwardings=yes".to_owned(),
        format!("KexAlgorithms={FLOOR_KEX_ALGORITHMS}"),
        format!("Ciphers={FLOOR_CIPHERS}"),
        format!("HostKeyAlgorithms={FLOOR_HOST_KEY_ALGORITHMS}"),
        format!("ConnectTimeout={connect_timeout}"),
    ]
}

#[test]
fn flag_args_floor_comes_first_in_order() {
    let cfg = default_config();
    let url = parse_url("ssh://example.com/srv/snap");
    let flags = cfg.flag_args(&url);
    let expected = floor_tokens(10);
    assert!(flags.len() >= expected.len() * 2);
    for (i, token) in expected.iter().enumerate() {
        assert_eq!(flags[2 * i], "-o");
        assert_eq!(&flags[2 * i + 1], token, "floor position {i}");
    }
    // Minimal config + URL: nothing after the floor.
    assert_eq!(flags.len(), expected.len() * 2);
}

#[test]
fn flag_args_url_port_beats_env_port() {
    let pairs = [("SNAPDIR_SSH_STORE_PORT", "2200")];
    let cfg = Config::from_lookup(Engine::Ssh, lookup_from(&pairs)).unwrap();

    let url = parse_url("ssh://example.com:9022/srv/snap");
    let flags = cfg.flag_args(&url);
    assert!(
        flags.contains(&"Port=9022".to_owned()),
        "URL port wins: {flags:?}"
    );
    assert!(!flags.contains(&"Port=2200".to_owned()));

    let url = parse_url("ssh://example.com/srv/snap");
    let flags = cfg.flag_args(&url);
    assert!(
        flags.contains(&"Port=2200".to_owned()),
        "env port is the fallback"
    );
}

#[test]
fn flag_args_config_derived_options() {
    let pairs = [
        ("SNAPDIR_SSH_STORE_IDENTITY_FILE", "/keys/id_ed25519"),
        ("SNAPDIR_SSH_STORE_KNOWN_HOSTS", "/keys/known_hosts"),
    ];
    let cfg = Config::from_lookup(Engine::Ssh, lookup_from(&pairs)).unwrap();
    let url = parse_url("ssh://deploy@example.com/srv/snap");
    let flags = cfg.flag_args(&url);
    assert!(flags.contains(&"User=deploy".to_owned()));
    let identity = flags
        .iter()
        .position(|f| f == "IdentityFile=/keys/id_ed25519")
        .expect("IdentityFile flag");
    assert_eq!(
        flags[identity + 2],
        "IdentitiesOnly=yes",
        "IdentitiesOnly rides with IdentityFile"
    );
    assert!(flags.contains(&"UserKnownHostsFile=/keys/known_hosts".to_owned()));

    // Without an identity file, IdentitiesOnly must NOT be forced.
    let cfg = default_config();
    let flags = cfg.flag_args(&url);
    assert!(!flags.iter().any(|f| f.starts_with("IdentitiesOnly")));
    assert!(!flags.iter().any(|f| f.starts_with("IdentityFile")));
}

#[test]
fn flag_args_extras_come_strictly_after_the_floor() {
    // The headline un-weakenability proof: a hostile extra trying to disable
    // host-key checking lands strictly AFTER the floor's `=yes` token, and
    // OpenSSH is first-obtained-value-wins.
    let pairs = [("SNAPDIR_SSH_STORE_EXTRA_OPTS", "StrictHostKeyChecking=no")];
    let cfg = Config::from_lookup(Engine::Ssh, lookup_from(&pairs)).unwrap();
    let url = parse_url("ssh://example.com/srv/snap");
    let flags = cfg.flag_args(&url);
    let floor_pos = flags
        .iter()
        .position(|f| f == "StrictHostKeyChecking=yes")
        .expect("floor token");
    let extra_pos = flags
        .iter()
        .position(|f| f == "StrictHostKeyChecking=no")
        .expect("extra token");
    assert!(
        floor_pos < extra_pos,
        "floor must precede the extra: {flags:?}"
    );
    assert_eq!(extra_pos, flags.len() - 1, "extras are last");
}

// ---------------------------------------------------------------------------
// version floor: `ssh -V` banner table
// ---------------------------------------------------------------------------

#[test]
fn openssh_version_parser_table() {
    assert_eq!(
        parse_openssh_version("OpenSSH_9.6p1, LibreSSL 3.3.6").unwrap(),
        (9, 6)
    );
    assert_eq!(
        parse_openssh_version("OpenSSH_8.4p1 Debian-5+deb11u3, OpenSSL 1.1.1n  15 Mar 2022")
            .unwrap(),
        (8, 4)
    );
    assert_eq!(parse_openssh_version("OpenSSH_8.5p1").unwrap(), (8, 5));
    assert_eq!(
        parse_openssh_version("OpenSSH_10.0p2, OpenSSL 3.5.0").unwrap(),
        (10, 0)
    );
    for garbage in [
        "",
        "ssh: command not found",
        "Dropbear v2022.83",
        "OpenSSH_for_Windows_8.1p1",
        "OpenSSH_x.y",
    ] {
        let err = parse_openssh_version(garbage).unwrap_err().to_string();
        assert!(err.contains("cannot parse"), "{garbage:?}: {err}");
    }
}

#[test]
fn openssh_floor_fails_closed_below_8_5() {
    assert_eq!(MIN_OPENSSH, (8, 5));
    let err = check_openssh_floor("OpenSSH_8.4p1 Debian-5")
        .unwrap_err()
        .to_string();
    assert!(err.contains("8.4") && err.contains("8.5"), "{err}");
    check_openssh_floor("OpenSSH_8.5p1").unwrap();
    check_openssh_floor("OpenSSH_9.9p2").unwrap();
    check_openssh_floor("OpenSSH_10.1p1").unwrap();
    // Unparsable banner fails closed too.
    check_openssh_floor("Dropbear v2022.83").unwrap_err();
}

// ---------------------------------------------------------------------------
// script: quoting, heredocs, sharded-path reuse, skeleton invariants
// ---------------------------------------------------------------------------

#[test]
fn sh_quote_edge_cases() {
    assert_eq!(sh_quote("plain"), "'plain'");
    assert_eq!(sh_quote("a b"), "'a b'");
    assert_eq!(sh_quote(""), "''");
    assert_eq!(sh_quote("don't"), r"'don'\''t'");
    assert_eq!(sh_quote("*?[!~"), "'*?[!~'");
    assert_eq!(sh_quote("$HOME `id`"), "'$HOME `id`'");
}

#[test]
fn sftp_quote_edge_cases() {
    assert_eq!(sftp_quote("plain"), "\"plain\"");
    assert_eq!(sftp_quote("a b/c"), "\"a b/c\"");
    assert_eq!(sftp_quote("a\"b"), "\"a\\\"b\"");
    assert_eq!(sftp_quote("a\\b"), "\"a\\\\b\"");
}

#[test]
fn heredoc_emits_quoted_delimiter_and_lines() {
    let out = heredoc("cat >\"$snapdir_tmp/list\"", &["one".into(), "two".into()]);
    assert_eq!(
        out,
        "cat >\"$snapdir_tmp/list\" <<'SNAPDIR_EOF'\none\ntwo\nSNAPDIR_EOF\n"
    );
}

#[test]
fn heredoc_delimiter_dodges_colliding_lines() {
    let out = heredoc(
        "cat",
        &["SNAPDIR_EOF".into(), "SNAPDIR_EOF_".into(), "x".into()],
    );
    let delimiter = "SNAPDIR_EOF__";
    assert!(out.starts_with(&format!("cat <<'{delimiter}'\n")), "{out}");
    assert!(out.ends_with(&format!("\n{delimiter}\n")));
}

#[test]
fn remote_paths_reuse_the_frozen_core_sharding() {
    // Golden values from snapdir_core::store's doc examples — the helpers
    // must delegate to the frozen layout, never reimplement it.
    let hex = "49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92";
    assert_eq!(
        remote_object_path("/srv/snap", hex),
        "/srv/snap/.objects/49d/c87/0df/1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92"
    );
    assert_eq!(
        remote_manifest_path("/srv/snap", hex),
        "/srv/snap/.manifests/49d/c87/0df/1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92"
    );
    assert_eq!(remote_object_path("/srv/snap", hex), {
        format!("/srv/snap/{}", snapdir_core::store::object_path(hex))
    });
}

/// Extracts the single-line body of a `_snapdir_<name>()` wrapper.
fn wrapper_body<'a>(script: &'a str, name: &str) -> &'a str {
    let open = format!("_snapdir_{name}() {{");
    let start = script
        .find(&open)
        .unwrap_or_else(|| panic!("{open} missing"));
    let rest = &script[start + open.len()..];
    let end = rest.find('}').expect("wrapper close");
    rest[..end].trim()
}

fn assert_ordered_floor(text: &str, context: &str) {
    let mut from = 0usize;
    for token in floor_tokens(10) {
        let quoted = format!("-o '{token}'");
        let pos = text[from..]
            .find(&quoted)
            .unwrap_or_else(|| panic!("{context}: {quoted} missing/out of order in: {text}"));
        from += pos + quoted.len();
    }
}

#[test]
fn skeleton_carries_cleanup_trap_mux_and_floor_in_every_wrapper() {
    let cfg = default_config();
    let url = parse_url("ssh://deploy@example.com/srv/snap");
    let script = skeleton(&url, &cfg);

    // Private 0700 temp dir + cleanup trap (EXIT TERM HUP — never INT).
    assert!(script.contains("mktemp -d"), "{script}");
    assert!(script.contains("chmod 700 \"$snapdir_tmp\""));
    assert!(script.contains("trap _snapdir_cleanup EXIT TERM HUP"));
    for line in script.lines() {
        if line.contains("trap") {
            assert!(
                !line.contains("INT"),
                "the orchestrator owns the INT trap: {line}"
            );
        }
    }

    // Cleanup closes the master and removes the temp dir.
    let cleanup = wrapper_body(&script, "cleanup");
    assert!(cleanup.contains("-O exit"), "{cleanup}");
    assert!(cleanup.contains("rm -rf \"$snapdir_tmp\""));

    // Both wrappers multiplex and carry the ordered floor + host separator.
    for (name, command) in [("ssh", "command ssh "), ("sftp", "command sftp ")] {
        let body = wrapper_body(&script, name);
        assert!(body.contains(command), "{name}: {body}");
        assert!(body.contains("-o ControlMaster=auto"), "{name}: {body}");
        assert!(
            body.contains("-o ControlPath=\"$snapdir_tmp/cm\""),
            "{name}: {body}"
        );
        assert!(body.contains("-o ControlPersist=60"), "{name}: {body}");
        assert_ordered_floor(body, name);
        assert!(body.contains("-- 'example.com'"), "{name}: {body}");
    }
    assert!(wrapper_body(&script, "ssh").ends_with("\"$@\""));
    assert!(wrapper_body(&script, "sftp").contains("-b \"$1\""));
    assert_ordered_floor(wrapper_body(&script, "cleanup"), "cleanup");
}

#[test]
fn skeleton_extras_render_after_the_floor_and_user_rides_along() {
    let pairs = [
        ("SNAPDIR_SSH_STORE_EXTRA_OPTS", "StrictHostKeyChecking=no"),
        ("SNAPDIR_SSH_STORE_CONTROL_PERSIST", "120"),
    ];
    let cfg = Config::from_lookup(Engine::Ssh, lookup_from(&pairs)).unwrap();
    let url = parse_url("ssh://deploy@[2001:db8::1]:2200/srv/snap");
    let script = skeleton(&url, &cfg);

    let body = wrapper_body(&script, "ssh");
    let floor = body.find("-o 'StrictHostKeyChecking=yes'").expect("floor");
    let extra = body.find("-o 'StrictHostKeyChecking=no'").expect("extra");
    assert!(floor < extra, "floor first, extras last: {body}");
    assert!(body.contains("-o 'Port=2200'"));
    assert!(body.contains("-o 'User=deploy'"));
    assert!(body.contains("-o ControlPersist=120"));
    assert!(
        body.contains("-- '[2001:db8::1]'"),
        "IPv6 stays bracketed: {body}"
    );
}

#[test]
fn skeleton_is_bash_32_clean() {
    let cfg = default_config();
    let url = parse_url("ssh://example.com/srv/snap");
    let script = skeleton(&url, &cfg);
    assert!(!script.contains("declare -A"), "no associative arrays");
    assert!(
        !script.contains("^^") && !script.contains(",,"),
        "no 4.x case ops"
    );
    assert!(!script.contains("readarray") && !script.contains("mapfile"));
}

// ---------------------------------------------------------------------------
// run dispatcher: version line, fail-closed engines, error surfacing
// ---------------------------------------------------------------------------

fn run_capture(engine: Engine, args: &[&str]) -> (u8, String, String) {
    // The dispatcher reads the process env (Config::from_env) — serialize
    // with the env-mutating tests.
    let _guard = ENV_LOCK.lock().unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_with(engine, os_args(args), std::io::empty(), &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn run_version_prints_engine_binary_name_and_crate_version() {
    let (code, out, err) = run_capture(Engine::Ssh, &["snapdir-ssh-store", "--version"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!("snapdir-ssh-store {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(err.is_empty());

    let (code, out, _) = run_capture(Engine::Sftp, &["snapdir-sftp-store", "-v"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!("snapdir-sftp-store {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn run_engines_fail_closed_until_implemented() {
    let (code, out, err) = run_capture(
        Engine::Ssh,
        &[
            "snapdir-ssh-store",
            "get-manifest-command",
            "--id",
            "abc",
            "--store",
            "ssh://example.com/srv/snap",
        ],
    );
    assert_eq!(code, 1);
    assert!(out.is_empty(), "stdout must stay script-pure: {out:?}");
    assert!(err.contains("not implemented"), "{err}");
    assert!(err.contains("get-manifest-command"), "{err}");
}

#[test]
fn run_surfaces_arg_and_url_errors_on_stderr() {
    let (code, _, err) = run_capture(Engine::Ssh, &["snapdir-ssh-store", "--bogus=1"]);
    assert_eq!(code, 1);
    assert!(err.contains("snapdir-ssh-store:"), "{err}");
    assert!(
        err.contains("missing subcommand") || err.contains("unknown option"),
        "{err}"
    );

    let (code, _, err) = run_capture(
        Engine::Sftp,
        &[
            "snapdir-sftp-store",
            "get-manifest-command",
            "--id",
            "abc",
            "--store",
            "ssh://example.com/srv/snap",
        ],
    );
    assert_eq!(code, 1);
    assert!(err.contains("sftp://"), "scheme mismatch surfaced: {err}");
}
