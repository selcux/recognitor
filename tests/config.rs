//! Config file, vault resolution and `config get/set` (doc 07 §12 "Which vault" and "Config file", doc 05 §5).
//! Nothing here reads or writes the per-user config: every path is in a temp folder, and the binary runs with
//! `RECOGNITOR_CONFIG` pointing there and with the other settings removed from its environment.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use recognitor::Error;
use recognitor::config::{self, KEYS};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Temp {
    base: PathBuf,
}

impl Temp {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-config-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        Temp { base }
    }

    /// A config file path in a folder that does not exist yet.
    fn config(&self) -> PathBuf {
        self.base.join("cfg").join("config.toml")
    }

    /// A small vault folder with one searchable note.
    fn vault(&self, name: &str) -> PathBuf {
        let root = self.base.join(name);
        fs::create_dir_all(root.join("common")).unwrap();
        fs::write(root.join("common").join("Live.md"), "# Live\n\nMarker zebra here.\n").unwrap();
        root
    }

    fn db(&self, name: &str) -> PathBuf {
        self.base.join(name)
    }

    /// The binary with a hermetic environment: the config file in the temp folder, no vault or database settings.
    fn recognitor(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_recognitor"));
        cmd.args(args)
            .env("RECOGNITOR_CONFIG", self.config())
            .env_remove("RECOGNITOR_VAULT")
            .env("RECOGNITOR_DB", self.db("env.db"));
        cmd
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn os(s: &str) -> Option<OsString> {
    Some(OsString::from(s))
}

/// Doc 07 §12 "Config file": `config.toml` in `ProjectDirs::config_local_dir()`, which is
/// `%LOCALAPPDATA%\recognitor\config\config.toml` on Windows. The path is only computed here, never touched.
#[test]
fn the_default_config_path_is_config_toml_in_the_local_config_folder() {
    let path = config::config_path_from(None).unwrap();
    let expected_suffix = if cfg!(windows) {
        Path::new("recognitor").join("config").join("config.toml")
    } else {
        Path::new("recognitor").join("config.toml")
    };
    assert!(path.ends_with(expected_suffix), "{path:?}");
    // The override replaces it whole.
    assert_eq!(
        config::config_path_from(os("/x/y.toml")).unwrap(),
        PathBuf::from("/x/y.toml")
    );
    assert_eq!(config::config_path_from(os("")).unwrap(), path);
}

/// Doc 07 §12 "Which vault": the flag, else `RECOGNITOR_VAULT`, else `vault.path`.
#[test]
fn the_vault_comes_from_the_flag_then_the_environment_then_the_config_file() {
    let t = Temp::new();
    let cfg = t.config();
    config::set(&cfg, "vault.path", t.vault("from-config").to_str().unwrap()).unwrap();
    let flag = Some(PathBuf::from("from-flag"));

    assert_eq!(
        config::resolve_vault(flag.clone(), os("from-env"), &cfg).unwrap(),
        PathBuf::from("from-flag")
    );
    assert_eq!(
        config::resolve_vault(None, os("from-env"), &cfg).unwrap(),
        PathBuf::from("from-env")
    );
    assert_eq!(
        config::resolve_vault(None, None, &cfg).unwrap(),
        t.base.join("from-config")
    );
    // An empty variable counts as unset.
    assert_eq!(
        config::resolve_vault(None, os(""), &cfg).unwrap(),
        t.base.join("from-config")
    );
}

/// Doc 07 §12 "Which vault": none of the three set is an error that names all three; the current folder is never
/// used.
#[test]
fn no_vault_names_all_three_places() {
    let t = Temp::new();
    let err = config::resolve_vault(None, None, &t.config()).unwrap_err();
    assert!(matches!(err, Error::NoVault { .. }));
    assert!(err.is_usage());
    let text = err.to_string();
    for needle in ["--vault", "RECOGNITOR_VAULT", "vault.path"] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
}

/// Doc 05 §5: `RECOGNITOR_DB`, else `db.path`, else the default place (`None`).
#[test]
fn the_database_comes_from_the_environment_then_the_config_file() {
    let t = Temp::new();
    let cfg = t.config();
    assert_eq!(config::resolve_db(None, &cfg).unwrap(), None);
    config::set(&cfg, "db.path", "somewhere/index.db").unwrap();
    assert_eq!(
        config::resolve_db(None, &cfg).unwrap(),
        Some(PathBuf::from("somewhere/index.db"))
    );
    assert_eq!(
        config::resolve_db(os(""), &cfg).unwrap(),
        Some(PathBuf::from("somewhere/index.db"))
    );
    assert_eq!(
        config::resolve_db(os("env.db"), &cfg).unwrap(),
        Some(PathBuf::from("env.db"))
    );
}

/// Doc 07 §12 "Config file": `set` creates the file and its folder, and keeps the other keys.
#[test]
fn set_creates_the_file_and_keeps_other_keys() {
    let t = Temp::new();
    let cfg = t.config();
    assert!(!cfg.parent().unwrap().exists());
    assert_eq!(config::get(&cfg, "db.path").unwrap(), None);

    config::set(&cfg, "db.path", "a.db").unwrap();
    assert!(cfg.is_file());
    let vault = t.vault("v");
    config::set(&cfg, "vault.path", vault.to_str().unwrap()).unwrap();
    assert_eq!(config::get(&cfg, "db.path").unwrap().as_deref(), Some("a.db"));
    assert_eq!(config::get(&cfg, "vault.path").unwrap().as_deref(), vault.to_str());

    // A key this version does not know, in the file already, survives a set.
    let text = fs::read_to_string(&cfg).unwrap();
    fs::write(&cfg, format!("{text}\n[read]\nmax_tokens = 900\n")).unwrap();
    config::set(&cfg, "db.path", "b.db").unwrap();
    let text = fs::read_to_string(&cfg).unwrap();
    assert!(text.contains("max_tokens = 900"), "{text}");
    assert_eq!(config::get(&cfg, "db.path").unwrap().as_deref(), Some("b.db"));
}

/// `set` changes only its own value: comments, spacing, unknown keys and the order of the file stay byte for byte.
#[test]
fn set_leaves_the_rest_of_a_hand_edited_file_as_it_was() {
    let t = Temp::new();
    let cfg = t.config();
    fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    let original = "# My settings, edited by hand\n\n[vault]\npath = \"old\"   # where the notes live\nother = 1\n\n\
                    [read]\nmax_tokens = 900 # an unknown key, kept\n\n[db]\npath='x.db'\n";
    fs::write(&cfg, original).unwrap();

    let vault = t.vault("v");
    config::set(&cfg, "vault.path", vault.to_str().unwrap()).unwrap();
    let after = fs::read_to_string(&cfg).unwrap();
    // The value is written as a basic string, or as a literal string when it holds backslashes (a Windows path).
    let path = vault.to_str().unwrap();
    let basic = format!("\"{}\"", path.replace('\\', "\\\\"));
    let literal = format!("'{path}'");
    let restored = after.replace(&literal, "\"old\"").replace(&basic, "\"old\"");
    assert_eq!(restored, original, "{after}");
    assert_eq!(config::get(&cfg, "vault.path").unwrap().as_deref(), vault.to_str());

    // Setting the same key again, and another key in another style, still changes only that value.
    config::set(&cfg, "db.path", "y.db").unwrap();
    let again = fs::read_to_string(&cfg).unwrap();
    assert_eq!(again.replace("path=\"y.db\"", "path='x.db'"), after);
    assert_eq!(config::get(&cfg, "db.path").unwrap().as_deref(), Some("y.db"));
}

/// A key that is not in the file yet is added to its section, or a new section is added at the end; the existing
/// text before it is not touched.
#[test]
fn set_adds_a_missing_key_without_touching_the_text_before_it() {
    let t = Temp::new();
    let cfg = t.config();
    fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    let original = "# comment\n[vault]\n# the notes\nunknown = true\n";
    fs::write(&cfg, original).unwrap();
    config::set(&cfg, "db.path", "z.db").unwrap();
    let after = fs::read_to_string(&cfg).unwrap();
    assert!(after.starts_with(original), "{after}");
    assert_eq!(config::get(&cfg, "db.path").unwrap().as_deref(), Some("z.db"));
}

/// Doc 07 §12: `vault.path` is stored as given, and must be an existing folder when set.
#[test]
fn vault_path_is_checked_and_stored_as_given() {
    let t = Temp::new();
    let cfg = t.config();
    let missing = t.base.join("nope");
    let err = config::set(&cfg, "vault.path", missing.to_str().unwrap()).unwrap_err();
    assert!(matches!(err, Error::InvalidConfigValue { .. }), "{err}");
    assert!(err.is_usage());
    assert!(err.to_string().contains("does not exist or is not a folder"), "{err}");
    assert!(!cfg.exists(), "a refused value must not create the file");

    // A file is not a folder.
    let file = t.base.join("a-file");
    fs::write(&file, "x").unwrap();
    assert!(config::set(&cfg, "vault.path", file.to_str().unwrap()).is_err());

    // Stored as given: a path with `..` is not normalised.
    t.vault("v");
    let given = format!("{}/../v", t.base.join("v").display());
    config::set(&cfg, "vault.path", &given).unwrap();
    assert_eq!(
        config::get(&cfg, "vault.path").unwrap().as_deref(),
        Some(given.as_str())
    );
}

/// Doc 07 §12: an unknown key is an error that lists the known keys.
#[test]
fn an_unknown_key_lists_the_known_ones() {
    let t = Temp::new();
    assert_eq!(
        KEYS,
        ["vault.path", "db.path", "bootstrap.max_tokens", "read.max_tokens"]
    );
    for result in [
        config::get(&t.config(), "vault.pth").map(|_| ()),
        config::set(&t.config(), "search.max_tokens", "9"),
    ] {
        let err = result.unwrap_err();
        assert!(matches!(err, Error::UnknownConfigKey(_)));
        assert!(err.is_usage());
        let text = err.to_string();
        assert!(
            text.contains("vault.path")
                && text.contains("db.path")
                && text.contains("bootstrap.max_tokens")
                && text.contains("read.max_tokens"),
            "{text}"
        );
    }
    assert!(!t.config().exists());
}

/// A config file that is not valid TOML is an error, and `set` does not overwrite it.
#[test]
fn a_broken_config_file_is_never_overwritten() {
    let t = Temp::new();
    let cfg = t.config();
    fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    fs::write(&cfg, "this is = = not toml [").unwrap();
    assert!(matches!(config::get(&cfg, "db.path"), Err(Error::Config { .. })));
    assert!(config::set(&cfg, "db.path", "x.db").is_err());
    assert_eq!(fs::read_to_string(&cfg).unwrap(), "this is = = not toml [");
    // A value of the wrong kind is reported too.
    fs::write(&cfg, "[db]\npath = 3\n").unwrap();
    assert!(matches!(config::get(&cfg, "db.path"), Err(Error::Config { .. })));
}

/// The CLI: `config set`, `config get`, errors and their exit codes.
#[test]
fn cli_config_get_and_set() {
    let t = Temp::new();
    let v = t.vault("v");

    let out = t
        .recognitor(&["config", "set", "vault.path", v.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(t.config().is_file());
    let out = t.recognitor(&["config", "get", "vault.path"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(stdout(&out), format!("{}\n", v.display()));

    // Unset key: an error, exit 1.
    let out = t.recognitor(&["config", "get", "db.path"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("db.path is not set"), "{}", stderr(&out));

    // Unknown key: a usage error listing the known keys, exit 2.
    let out = t.recognitor(&["config", "get", "nope.key"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("vault.path, db.path"), "{}", stderr(&out));

    // A vault folder that does not exist: refused, exit 2.
    let out = t
        .recognitor(&["config", "set", "vault.path", "/no/such/folder"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));

    // Wrong shapes.
    for args in [
        vec!["config"],
        vec!["config", "get"],
        vec!["config", "set", "db.path"],
        vec!["config", "frob", "x"],
    ] {
        let out = t.recognitor(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
}

/// The CLI: no vault anywhere is a usage error that names all three places.
#[test]
fn cli_without_a_vault_names_all_three_places() {
    let t = Temp::new();
    for args in [vec!["search", "zebra"], vec!["sync"], vec!["rebuild"]] {
        let out = t.recognitor(&args).current_dir(&t.base).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let text = stderr(&out);
        for needle in ["--vault", "RECOGNITOR_VAULT", "vault.path"] {
            assert!(text.contains(needle), "{args:?} {needle}: {text}");
        }
        assert!(out.stdout.is_empty());
    }
}

/// The CLI: the vault from the flag, the variable and the config file, in that order. The current folder is not
/// used, even when it is a vault.
#[test]
fn cli_resolves_the_vault_in_order() {
    let t = Temp::new();
    let from_flag = t.vault("flag");
    let from_env = t.vault("env");
    let from_config = t.vault("config");
    fs::write(
        from_flag.join("common").join("Live.md"),
        "# Live\n\nMarker zebra from the flag.\n",
    )
    .unwrap();
    fs::write(
        from_env.join("common").join("Live.md"),
        "# Live\n\nMarker zebra from the variable.\n",
    )
    .unwrap();
    fs::write(
        from_config.join("common").join("Live.md"),
        "# Live\n\nMarker zebra from the config.\n",
    )
    .unwrap();
    config::set(&t.config(), "vault.path", from_config.to_str().unwrap()).unwrap();

    let search = |t: &Temp, vault_flag: Option<&Path>, env: Option<&Path>, db: &str| {
        let mut cmd = t.recognitor(&["search", "zebra"]);
        cmd.env("RECOGNITOR_DB", t.db(db));
        if let Some(f) = vault_flag {
            cmd.arg("--vault").arg(f);
        }
        if let Some(e) = env {
            cmd.env("RECOGNITOR_VAULT", e);
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        stdout(&out)
    };
    assert!(search(&t, Some(&from_flag), Some(&from_env), "1.db").contains("from the flag"));
    assert!(search(&t, None, Some(&from_env), "2.db").contains("from the variable"));
    assert!(search(&t, None, None, "3.db").contains("from the config"));

    // The current folder is a vault with a note, and still is not used.
    let here = t.vault("here");
    let out = t.recognitor(&["search", "zebra"]).current_dir(&here).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("from the config"), "{}", stdout(&out));
}

/// The CLI: `db.path` in the config file sets the database, below `RECOGNITOR_DB`.
#[test]
fn cli_database_from_the_config_file() {
    let t = Temp::new();
    let v = t.vault("v");
    let from_config = t.db("from-config.db");
    let from_env = t.db("from-env.db");
    config::set(&t.config(), "db.path", from_config.to_str().unwrap()).unwrap();

    let out = t
        .recognitor(&["search", "zebra", "--vault", v.to_str().unwrap()])
        .env_remove("RECOGNITOR_DB")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(from_config.is_file());

    let out = t
        .recognitor(&["search", "zebra", "--vault", v.to_str().unwrap()])
        .env("RECOGNITOR_DB", &from_env)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(from_env.is_file());
}
