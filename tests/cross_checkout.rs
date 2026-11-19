// Copyright 2026 sccache contributors
// Licensed under the Apache License, Version 2.0.

#![cfg(unix)]

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use test_case::test_case;

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    compiler: PathBuf,
    cmake: PathBuf,
    ninja: PathBuf,
    client_side: bool,
}

impl Fixture {
    fn new(client_side: bool) -> Option<Self> {
        let (Ok(compiler), Ok(cmake), Ok(ninja)) = (
            which::which("clang++"),
            which::which("cmake"),
            which::which("ninja"),
        ) else {
            eprintln!("cross-checkout test needs clang++, cmake and ninja");
            return None;
        };
        let temp = tempfile::Builder::new()
            .prefix("sccx-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::write(root.join("config.toml"), "").unwrap();
        let this = Self {
            _temp: temp,
            root,
            compiler,
            cmake,
            ninja,
            client_side,
        };
        this.run(
            this.command(env!("CARGO_BIN_EXE_sccache"))
                .arg("--start-server"),
        );
        Some(this)
    }

    fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command.env_clear();
        for key in [
            "PATH",
            "HOME",
            "TMPDIR",
            "DEVELOPER_DIR",
            "SDKROOT",
            "MACOSX_DEPLOYMENT_TARGET",
        ] {
            if let Some(value) = env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("SCCACHE_DIR", self.root.join("cache"))
            .env("SCCACHE_CONF", self.root.join("config.toml"))
            .env("SCCACHE_CACHED_CONF", self.root.join("cached.toml"))
            .env("SCCACHE_SERVER_UDS", self.root.join("server.sock"))
            .env("SCCACHE_DIRECT", "true")
            .env(
                "SCCACHE_CLIENT_SIDE",
                if self.client_side { "1" } else { "0" },
            );
        command
    }

    fn run(&self, command: &mut Command) -> Output {
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{command:?}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn roots(&self, command: &mut Command, source: &Path, build: &Path) {
        command.env(
            "SCCACHE_BASEDIRS",
            env::join_paths([source, build]).unwrap(),
        );
    }

    fn configure(&self, source: &Path, build: &Path) {
        let mut command = self.command(&self.cmake);
        command
            .args(["-G", "Ninja", "-S"])
            .arg(source)
            .arg("-B")
            .arg(build)
            .arg(format!("-DCMAKE_CXX_COMPILER={}", self.compiler.display()))
            .arg(format!(
                "-DCMAKE_CXX_COMPILER_LAUNCHER={}",
                env!("CARGO_BIN_EXE_sccache")
            ));
        self.roots(&mut command, source, build);
        self.run(&mut command);
    }

    fn build(&self, source: &Path, build: &Path) -> String {
        let mut command = self.command(&self.ninja);
        command.arg("-C").arg(build);
        self.roots(&mut command, source, build);
        String::from_utf8(self.run(&mut command).stdout).unwrap()
    }

    fn output(&self, program: &Path) -> String {
        String::from_utf8(self.run(&mut self.command(program)).stdout).unwrap()
    }

    fn hits(&self) -> u64 {
        let output = self.run(self.command(env!("CARGO_BIN_EXE_sccache")).args([
            "--show-stats",
            "--stats-format",
            "json",
        ]));
        let stats: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        stats["stats"]["cache_hits"]["counts"]["C/C++"]
            .as_u64()
            .unwrap_or(0)
    }

    fn source(&self, name: &str, absolute_include: bool, literal: bool) -> PathBuf {
        let source = self.root.join(name);
        std::fs::create_dir_all(&source).unwrap();
        let header = source.join("config.hpp");
        std::fs::write(
            &header,
            "#pragma once\ninline int answer() { return 42; }\n",
        )
        .unwrap();
        let include = if absolute_include {
            header.display().to_string()
        } else {
            "config.hpp".to_owned()
        };
        let main = if literal {
            format!(
                "#include <cstdio>\nint main() {{ std::puts(\"{}/\"); }}\n",
                source.display()
            )
        } else {
            format!(
                "#include \"{include}\"\n#include <cstdio>\nint main() {{ std::printf(\"%d\\n\", answer()); }}\n"
            )
        };
        std::fs::write(source.join("main.cpp"), main).unwrap();
        std::fs::write(source.join("CMakeLists.txt"), r#"cmake_minimum_required(VERSION 3.22)
project(CrossCheckout LANGUAGES CXX)
set(CMAKE_BUILD_TYPE Debug)
add_compile_options("-ffile-prefix-map=${CMAKE_BINARY_DIR}=/build" "-ffile-prefix-map=${CMAKE_SOURCE_DIR}=/source" -fdebug-compilation-dir=/build)
add_executable(probe main.cpp)
"#).unwrap();
        let old = filetime::FileTime::from_system_time(SystemTime::now() - Duration::from_secs(30));
        for file in [header, source.join("main.cpp")] {
            filetime::set_file_times(file, old, old).unwrap();
        }
        source
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self
            .command(env!("CARGO_BIN_EXE_sccache"))
            .arg("--stop-server")
            .output();
    }
}

#[test_case(false; "daemon")]
#[test_case(true; "client")]
fn cross_checkout_headers_and_depfiles(client_side: bool) {
    let Some(f) = Fixture::new(client_side) else {
        return;
    };
    let a = f.source("source A", false, false);
    let b = f.source("deeper/source B", false, false);
    let ba = f.root.join("build A");
    let bb = f.root.join("external/deep/build B");
    f.configure(&a, &ba);
    f.configure(&b, &bb);
    f.build(&a, &ba);
    let hits = f.hits();
    f.build(&b, &bb);
    assert_eq!(
        f.hits(),
        hits + 1,
        "first B preprocessing should find the shared object"
    );
    assert_eq!(f.output(&bb.join("probe")), "42\n");
    let deps = String::from_utf8(
        f.run(f.command(&f.ninja).arg("-C").arg(&bb).args(["-t", "deps"]))
            .stdout,
    )
    .unwrap();
    assert!(
        deps.contains(&b.join("config.hpp").display().to_string()),
        "{deps}"
    );
    assert!(!deps.contains(&a.display().to_string()), "{deps}");

    let object = bb.join("CMakeFiles/probe.dir/main.cpp.o");
    std::fs::remove_file(&object).unwrap();
    f.build(&b, &bb);
    let deps = String::from_utf8(
        f.run(f.command(&f.ninja).arg("-C").arg(&bb).args(["-t", "deps"]))
            .stdout,
    )
    .unwrap();
    assert!(
        deps.contains(&b.join("config.hpp").display().to_string()),
        "{deps}"
    );
    assert!(
        !deps.contains(&a.display().to_string()),
        "direct replay restored a foreign depfile: {deps}"
    );
    let header = b.join("config.hpp");
    std::fs::write(
        &header,
        "#pragma once\ninline int answer() { return 43; }\n",
    )
    .unwrap();
    let now = SystemTime::now();
    filetime::set_file_mtime(
        &object,
        filetime::FileTime::from_system_time(now - Duration::from_secs(20)),
    )
    .unwrap();
    filetime::set_file_mtime(
        &header,
        filetime::FileTime::from_system_time(now - Duration::from_secs(10)),
    )
    .unwrap();
    f.build(&b, &bb);
    assert_eq!(f.output(&bb.join("probe")), "43\n");
    std::fs::remove_file(&object).unwrap();
    f.build(&b, &bb);
    assert_eq!(f.output(&bb.join("probe")), "43\n");
}

#[test_case(false; "daemon")]
#[test_case(true; "client")]
fn cross_checkout_absolute_includes_share_objects(client_side: bool) {
    let Some(f) = Fixture::new(client_side) else {
        return;
    };
    let a = f.source("A", true, false);
    let b = f.source("B", true, false);
    let ba = f.root.join("build-a");
    let bb = f.root.join("out/build-b");
    f.configure(&a, &ba);
    f.configure(&b, &bb);
    f.build(&a, &ba);
    let hits = f.hits();
    // The old source tree is genuinely unavailable, including its absolute include.
    std::fs::rename(&a, f.root.join("A-unavailable")).unwrap();
    f.build(&b, &bb);
    assert_eq!(f.hits(), hits + 1);
    assert_eq!(f.output(&bb.join("probe")), "42\n");
}

#[test_case(false; "daemon")]
#[test_case(true; "client")]
fn cross_checkout_runtime_paths_are_not_normalized(client_side: bool) {
    let Some(f) = Fixture::new(client_side) else {
        return;
    };
    let a = f.source("A", false, true);
    let b = f.source("B", false, true);
    let ba = f.root.join("build-a");
    let bb = f.root.join("build-b");
    f.configure(&a, &ba);
    f.configure(&b, &bb);
    f.build(&a, &ba);
    let hits = f.hits();
    f.build(&b, &bb);
    assert_eq!(f.hits(), hits);
    assert_eq!(f.output(&bb.join("probe")), format!("{}/\n", b.display()));
}

fn native_compile(f: &Fixture, source: &Path, build: &Path, pch: bool, object: &str) {
    std::fs::create_dir_all(build).unwrap();
    let mut command = f.command(env!("CARGO_BIN_EXE_sccache"));
    command
        .arg(&f.compiler)
        .args([
            "-std=c++17",
            "-g",
            "-Xclang",
            "-fno-pch-timestamp",
            "-fpch-validate-input-files-content",
        ])
        .arg(format!("-ffile-prefix-map={}=/source", source.display()))
        .arg(format!("-ffile-prefix-map={}=/build", build.display()))
        .arg("-fdebug-compilation-dir=/build")
        .args(["-MD", "-MF"])
        .arg(build.join(format!("{object}.d")));
    if let Some(sdk) = env::var_os("SDKROOT") {
        command.arg("-isysroot").arg(sdk);
    }
    if pch {
        command
            .args(["-x", "c++-header", "-c"])
            .arg(source.join("config.hpp"));
    } else {
        command
            .arg("-include-pch")
            .arg(build.join("prefix.pch"))
            .arg("-c")
            .arg(source.join("main.cpp"));
    }
    command.arg("-o").arg(build.join(object)).current_dir(build);
    f.roots(&mut command, source, build);
    f.run(&mut command);
}

fn link_native(f: &Fixture, build: &Path, object: &str) {
    f.run(
        f.command(&f.compiler)
            .arg(build.join(object))
            .arg("-o")
            .arg(build.join("probe")),
    );
}

#[test_case(false; "daemon")]
#[test_case(true; "client")]
fn cross_checkout_native_pch_stays_local(client_side: bool) {
    let Some(f) = Fixture::new(client_side) else {
        return;
    };
    let a = f.source("A", false, false);
    let b = f.source("B", false, false);
    let ba = f.root.join("out-a");
    let bb = f.root.join("deeper/out-b");
    native_compile(&f, &a, &ba, true, "prefix.pch");
    native_compile(&f, &a, &ba, false, "main.o");
    let pch_a = std::fs::read(ba.join("prefix.pch")).unwrap();
    let hits = f.hits();
    std::fs::rename(&a, f.root.join("A-unavailable")).unwrap();
    std::fs::rename(&ba, f.root.join("out-a-unavailable")).unwrap();
    native_compile(&f, &b, &bb, true, "prefix.pch");
    assert_eq!(
        f.hits(),
        hits,
        "a binary PCH must not be restored from another root"
    );
    assert_ne!(pch_a, std::fs::read(bb.join("prefix.pch")).unwrap());
    native_compile(&f, &b, &bb, false, "main.o");
    assert_eq!(
        f.hits(),
        hits + 1,
        "PCH consumers may still share their objects"
    );
    link_native(&f, &bb, "main.o");
    assert_eq!(f.output(&bb.join("probe")), "42\n");
    std::fs::write(
        b.join("config.hpp"),
        "#pragma once\ninline int answer() { return 43; }\n",
    )
    .unwrap();
    native_compile(&f, &b, &bb, true, "prefix.pch");
    native_compile(&f, &b, &bb, false, "main.o");
    link_native(&f, &bb, "main.o");
    assert_eq!(f.output(&bb.join("probe")), "43\n");
}

#[test_case(false; "daemon")]
#[test_case(true; "client")]
fn local_direct_hit_cannot_replay_another_output_depfile(client_side: bool) {
    let Some(f) = Fixture::new(client_side) else {
        return;
    };
    let source = f.source("source", false, false);
    let build = f.root.join("output");
    native_compile(&f, &source, &build, true, "prefix.pch");
    native_compile(&f, &source, &build, false, "first.o");
    let hits = f.hits();
    native_compile(&f, &source, &build, false, "second.o");
    assert_eq!(f.hits(), hits + 1);
    std::fs::remove_file(build.join("second.o")).unwrap();
    native_compile(&f, &source, &build, false, "second.o");
    let depfile = std::fs::read_to_string(build.join("second.o.d")).unwrap();
    assert!(
        depfile.starts_with(&format!("{}:", build.join("second.o").display())),
        "{depfile}"
    );
}
