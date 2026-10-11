#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::Command;

pub const PASSWORD: &str = "S3cret-Pässword!";

pub struct Env {
    pub tmp: tempfile::TempDir,
    pub project: PathBuf,
    pub home: PathBuf,
    pub claude: PathBuf,
}

pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn all(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

impl Env {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("Sample App ✓ ünïcode");
        let home = tmp.path().join("clauck home");
        let claude = tmp.path().join("claude config");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::create_dir_all(project.join("tests")).unwrap();
        fs::write(
            project.join("src/app.py"),
            "def add(a, b):\n    return a + b\n",
        )
        .unwrap();
        fs::write(
            project.join("tests/test_app.py"),
            "from src.app import add\nassert add(1, 2) == 3\n",
        )
        .unwrap();
        fs::write(
            project.join("pyproject.toml"),
            "[project]\nname = \"sample\"\n",
        )
        .unwrap();
        fs::write(project.join("Makefile"), "test:\n\tpython -m pytest\n").unwrap();
        Self {
            tmp,
            project,
            home,
            claude,
        }
    }

    pub fn with_instructions(self) -> Self {
        fs::write(
            self.project.join("AGENTS.md"),
            "# Team guide\n\nRun `make test` before sending changes.\n",
        )
        .unwrap();
        fs::write(
            self.project.join("CLAUDE.md"),
            "# Claude notes\r\n\r\nPrefer small diffs.\r\n",
        )
        .unwrap();
        self
    }

    pub fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("clauck").unwrap();
        c.env("CLAUCK_HOME", &self.home)
            .env("CLAUDE_CONFIG_DIR", &self.claude)
            .env_remove("CLAUCK_NONINTERACTIVE")
            .arg("--path")
            .arg(&self.project)
            .timeout(Duration::from_secs(60));
        c
    }

    pub fn run(&self, args: &[&str]) -> Out {
        self.run_stdin(args, None)
    }

    pub fn run_stdin(&self, args: &[&str], stdin: Option<&str>) -> Out {
        let mut c = self.cmd();
        c.args(args);
        if let Some(s) = stdin {
            c.write_stdin(s.to_string());
        }
        let out = c.output().unwrap();
        Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    pub fn ok(&self, args: &[&str]) -> Out {
        let out = self.run(args);
        assert_eq!(out.code, 0, "clauck {args:?} failed:\n{}", out.all());
        out
    }

    pub fn ok_stdin(&self, args: &[&str], stdin: &str) -> Out {
        let out = self.run_stdin(args, Some(stdin));
        assert_eq!(out.code, 0, "clauck {args:?} failed:\n{}", out.all());
        out
    }

    pub fn lock_with_password(&self) -> Out {
        self.ok_stdin(
            &["lock", "--password", "--password-stdin"],
            &format!("{PASSWORD}\n{PASSWORD}\n"),
        )
    }

    pub fn status_json(&self) -> serde_json::Value {
        let out = self.ok(&["status", "--json"]);
        serde_json::from_str(&out.stdout).unwrap()
    }

    pub fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.project.join(rel)).unwrap()
    }

    pub fn write(&self, rel: &str, text: &str) {
        fs::write(self.project.join(rel), text).unwrap();
    }

    pub fn app_snapshot(&self) -> BTreeMap<String, Vec<u8>> {
        snapshot(&self.project, &["AGENTS.md", "CLAUDE.md", ".clauck"])
    }

    pub fn all_project_bytes(&self) -> Vec<u8> {
        snapshot(&self.project, &[])
            .into_values()
            .flatten()
            .collect()
    }
}

pub fn snapshot(root: &Path, skip: &[&str]) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    walk(root, root, skip, &mut out);
    out
}

fn walk(root: &Path, dir: &Path, skip: &[&str], out: &mut BTreeMap<String, Vec<u8>>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if skip
            .iter()
            .any(|s| rel == *s || rel.starts_with(&format!("{s}/")))
        {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, skip, out);
        } else {
            out.insert(rel, fs::read(&path).unwrap());
        }
    }
}

pub fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}
