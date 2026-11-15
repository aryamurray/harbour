//! Which linker driver a final link uses when the C++ is in a *dependency*.
//!
//! A C program that links a C++ static library needs the C++ runtime
//! (`__gxx_personality_v0`, `operator new`, ...), which only the C++ driver
//! supplies. The driver used to be chosen from the linked target's own
//! language, so a C executable over a C++ archive failed to link. The C++
//! below throws and uses `std::vector` so that the wrong driver genuinely
//! fails, rather than passing because nothing needed the runtime.
//!
//! The driver is asserted from the `-v` line `Linking executable ... (driver:
//! c++)`. That line reports Harbour's decision, not a binary name, so it reads
//! the same under MSVC (where `link.exe` serves both and the decision is
//! harmless); the functional assertion -- the program builds and prints the
//! right number -- is made on every platform.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use crate::common::*;

const CXX_HEADER: &str = "#ifndef CXXLIB_H\n#define CXXLIB_H\n\
    #ifdef __cplusplus\nextern \"C\" {\n#endif\n\
    int cxx_sum(int n);\n\
    #ifdef __cplusplus\n}\n#endif\n#endif\n";

/// Sums 0..n through a `std::vector`, and reports a negative `n` by throwing
/// and catching -- 10 for `cxx_sum(5)`.
const CXX_SOURCE: &str = "#include <stdexcept>\n#include <vector>\n#include \"cxxlib.h\"\n\
    extern \"C\" int cxx_sum(int n) {\n\
        std::vector<int> v;\n\
        try {\n\
            if (n < 0) throw std::runtime_error(\"negative\");\n\
            for (int i = 0; i < n; ++i) v.push_back(i);\n\
        } catch (const std::exception &) {\n\
            return -1;\n\
        }\n\
        int s = 0;\n\
        for (int x : v) s += x;\n\
        return s;\n\
    }\n";

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Package `cxxlib`: one C++ static library exporting `cxx_sum`.
fn write_cxxlib(root: &Path, lang: &str) {
    write(&root.join("src/cxxlib.h"), CXX_HEADER);
    write(&root.join("src/cxxlib.cc"), CXX_SOURCE);
    write(
        &root.join("Harbour.toml"),
        &format!(
            "[package]\nname = \"cxxlib\"\nversion = \"0.1.0\"\n\n\
             [targets.cxxlib]\nkind = \"staticlib\"\nlang = \"{lang}\"\n\
             sources = [\"src/cxxlib.cc\"]\n\n\
             [targets.cxxlib.surface.compile.public]\ninclude_dirs = [\"src\"]\n"
        ),
    );
}

/// Package `capp`: a C executable printing `cxx_sum(5)` through `dep`.
fn write_capp(root: &Path, dep: &str, body: &str) {
    write(
        &root.join("src/main.c"),
        &format!("#include <stdio.h>\n{body}"),
    );
    write(
        &root.join("Harbour.toml"),
        &format!(
            "[package]\nname = \"capp\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\n{dep} = {{ path = \"../{dep}\" }}\n\n\
             [targets.capp]\nkind = \"exe\"\nlang = \"c\"\nsources = [\"src/main.c\"]\n"
        ),
    );
}

const MAIN_CXX_SUM: &str = "#include \"cxxlib.h\"\n\
    int main(void) { printf(\"%d\\n\", cxx_sum(5)); return 0; }\n";

/// The `driver:` the `-v` output reports for the link of `name`.
fn link_driver(log: &RunLog, what: &str, name: &str) -> Option<String> {
    let text = log.combined();
    let line = text
        .lines()
        .find(|l| l.contains(what) && l.contains(name))?;
    let rest = line.split("(driver: ").nth(1)?;
    Some(rest.trim_end_matches(')').trim().to_string())
}

fn build_verbose(home: &Path, dir: &Path) -> RunLog {
    harbour_run(home, dir, &["build", "-v"]).success()
}

fn assert_driver(log: &RunLog, what: &str, name: &str, expected: &str) {
    assert_eq!(
        link_driver(log, what, name).as_deref(),
        Some(expected),
        "`{name}` must be linked with the {expected} driver\n{log}"
    );
}

/// The reported bug: a C executable over a C++ static library.
#[test]
fn test_c_exe_over_a_cxx_static_library_links_with_the_cxx_driver() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_cxxlib(&tmp.path().join("cxxlib"), "c++");
    let app = tmp.path().join("capp");
    write_capp(&app, "cxxlib", MAIN_CXX_SUM);

    let log = build_verbose(&home, &app);
    assert_driver(&log, "Linking executable", "capp", "c++");
    assert_eq!(run_built_exe(&app, "capp").out(), "10");
}

/// C exe -> C static library -> C++ static library: the C++ is two packages
/// away and the executable's own `deps` never names it.
#[test]
fn test_cxx_reached_through_a_c_static_library_still_selects_the_cxx_driver() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_cxxlib(&tmp.path().join("cxxlib"), "c++");

    let mid = tmp.path().join("clib");
    write(
        &mid.join("src/clib.h"),
        "#ifndef CLIB_H\n#define CLIB_H\nint clib_value(void);\n#endif\n",
    );
    write(
        &mid.join("src/clib.c"),
        "#include \"clib.h\"\n#include \"cxxlib.h\"\n\
         int clib_value(void) { return cxx_sum(5) + 1; }\n",
    );
    write(
        &mid.join("Harbour.toml"),
        "[package]\nname = \"clib\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\ncxxlib = { path = \"../cxxlib\" }\n\n\
         [targets.clib]\nkind = \"staticlib\"\nlang = \"c\"\nsources = [\"src/clib.c\"]\n\n\
         [targets.clib.surface.compile.public]\ninclude_dirs = [\"src\"]\n",
    );

    let app = tmp.path().join("capp");
    write_capp(
        &app,
        "clib",
        "#include \"clib.h\"\nint main(void) { printf(\"%d\\n\", clib_value()); return 0; }\n",
    );

    let log = build_verbose(&home, &app);
    assert_driver(&log, "Linking executable", "capp", "c++");
    assert_eq!(run_built_exe(&app, "capp").out(), "11");
}

/// The guard against over-triggering: no C++ anywhere keeps the C driver.
#[test]
fn test_a_pure_c_graph_keeps_the_c_driver() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let lib = tmp.path().join("clib");
    write(
        &lib.join("src/clib.h"),
        "#ifndef CLIB_H\n#define CLIB_H\nint clib_value(void);\n#endif\n",
    );
    write(
        &lib.join("src/clib.c"),
        "#include \"clib.h\"\nint clib_value(void) { return 7; }\n",
    );
    write(
        &lib.join("Harbour.toml"),
        "[package]\nname = \"clib\"\nversion = \"0.1.0\"\n\n\
         [targets.clib]\nkind = \"staticlib\"\nlang = \"c\"\nsources = [\"src/clib.c\"]\n\n\
         [targets.clib.surface.compile.public]\ninclude_dirs = [\"src\"]\n",
    );
    let app = tmp.path().join("capp");
    write_capp(
        &app,
        "clib",
        "#include \"clib.h\"\nint main(void) { printf(\"%d\\n\", clib_value()); return 0; }\n",
    );

    let log = build_verbose(&home, &app);
    assert_driver(&log, "Linking executable", "capp", "c");
    assert_eq!(run_built_exe(&app, "capp").out(), "7");
}

/// The same rule across `local = true` siblings of ONE package.
#[test]
fn test_c_exe_over_a_cxx_sibling_target_selects_the_cxx_driver() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let root = tmp.path().join("one");
    write(&root.join("src/cxxlib.h"), CXX_HEADER);
    write(&root.join("src/cxxlib.cc"), CXX_SOURCE);
    write(
        &root.join("src/main.c"),
        &format!("#include <stdio.h>\n{MAIN_CXX_SUM}"),
    );
    write(
        &root.join("Harbour.toml"),
        "[package]\nname = \"one\"\nversion = \"0.1.0\"\n\n\
         [targets.capp]\nkind = \"exe\"\nlang = \"c\"\nsources = [\"src/main.c\"]\n\n\
         [targets.capp.deps]\ncxxlib = { local = true }\n\n\
         [targets.cxxlib]\nkind = \"staticlib\"\nlang = \"c++\"\nsources = [\"src/cxxlib.cc\"]\n\n\
         [targets.cxxlib.surface.compile.public]\ninclude_dirs = [\"src\"]\n",
    );

    let log = build_verbose(&home, &root);
    assert_driver(&log, "Linking executable", "capp", "c++");
    assert_eq!(run_built_exe(&root, "capp").out(), "10");
}

/// A C shared library that statically absorbs a C++ archive needs the C++
/// driver itself, and a C executable over it then runs. Unix only: the
/// executable finds the library through the loader search path set below,
/// and a Windows DLL would need export annotations this fixture avoids.
#[cfg(unix)]
#[test]
fn test_c_shared_library_absorbing_a_cxx_static_library_selects_the_cxx_driver() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let cxx = tmp.path().join("cxxlib");
    write_cxxlib(&cxx, "c++");
    // Harbour does not add -fPIC on its own, and objects absorbed into a
    // shared library need it.
    let manifest = fs::read_to_string(cxx.join("Harbour.toml")).unwrap();
    write(
        &cxx.join("Harbour.toml"),
        &format!("{manifest}\n[targets.cxxlib.surface.compile.private]\ncflags = [\"-fPIC\"]\n"),
    );

    let sh = tmp.path().join("cshared");
    write(
        &sh.join("src/cshared.h"),
        "#ifndef CSHARED_H\n#define CSHARED_H\nint cshared_value(void);\n#endif\n",
    );
    write(
        &sh.join("src/cshared.c"),
        "#include \"cshared.h\"\n#include \"cxxlib.h\"\n\
         int cshared_value(void) { return cxx_sum(5) + 2; }\n",
    );
    write(
        &sh.join("Harbour.toml"),
        "[package]\nname = \"cshared\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\ncxxlib = { path = \"../cxxlib\" }\n\n\
         [targets.cshared]\nkind = \"sharedlib\"\nlang = \"c\"\nsources = [\"src/cshared.c\"]\n\n\
         [targets.cshared.surface.compile.public]\ninclude_dirs = [\"src\"]\n",
    );

    let app = tmp.path().join("capp");
    write_capp(
        &app,
        "cshared",
        "#include \"cshared.h\"\nint main(void) { printf(\"%d\\n\", cshared_value()); return 0; }\n",
    );

    let log = build_verbose(&home, &app);
    assert_driver(&log, "Creating shared library", "cshared", "c++");
    // The consumer links only a shared library: nothing C++ is statically
    // linked into it, so it keeps the C driver.
    assert_driver(&log, "Linking executable", "capp", "c");

    let lib_dir = built_exe_path(&app, "capp")
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let mut dirs: Vec<PathBuf> = snapshot_tree(&target_dir(&app))
        .into_keys()
        .filter(|p| p.extension().is_some_and(|e| e == "so" || e == "dylib"))
        .filter_map(|p| p.parent().map(Path::to_path_buf))
        .collect();
    dirs.push(lib_dir);
    dirs.sort();
    dirs.dedup();
    let search = std::env::join_paths(dirs).unwrap();
    let exe = built_exe_path(&app, "capp");
    let run = RunLog::capture(
        exe.display().to_string(),
        Command::new(&exe)
            .env("LD_LIBRARY_PATH", &search)
            .env("DYLD_LIBRARY_PATH", &search),
    )
    .success();
    assert_eq!(run.out(), "12");
}

/// Changing a dependency between C and C++ changes the driver, and the link
/// step must be redone with the other one: the driver is part of what the
/// link fingerprint covers.
#[test]
fn test_link_is_redone_when_a_dependency_changes_between_c_and_cxx() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    let dep = tmp.path().join("cxxlib");
    // A body that is valid C and C++, so flipping `lang` is the only change.
    write(&dep.join("src/cxxlib.h"), CXX_HEADER);
    write(
        &dep.join("src/cxxlib.c"),
        "#include \"cxxlib.h\"\nint cxx_sum(int n) { return n * 2; }\n",
    );
    let manifest = |lang: &str| {
        format!(
            "[package]\nname = \"cxxlib\"\nversion = \"0.1.0\"\n\n\
             [targets.cxxlib]\nkind = \"staticlib\"\nlang = \"{lang}\"\n\
             sources = [\"src/cxxlib.c\"]\n\n\
             [targets.cxxlib.surface.compile.public]\ninclude_dirs = [\"src\"]\n"
        )
    };
    write(&dep.join("Harbour.toml"), &manifest("c"));
    let app = tmp.path().join("capp");
    write_capp(&app, "cxxlib", MAIN_CXX_SUM_C);

    let first = build_verbose(&home, &app);
    assert_driver(&first, "Linking executable", "capp", "c");
    assert_eq!(run_built_exe(&app, "capp").out(), "10");

    write(&dep.join("Harbour.toml"), &manifest("c++"));
    let second = build_verbose(&home, &app);
    assert_driver(&second, "Linking executable", "capp", "c++");
    assert_eq!(run_built_exe(&app, "capp").out(), "10");

    write(&dep.join("Harbour.toml"), &manifest("c"));
    let third = build_verbose(&home, &app);
    assert_driver(&third, "Linking executable", "capp", "c");
}

const MAIN_CXX_SUM_C: &str = "#include \"cxxlib.h\"\n\
    int main(void) { printf(\"%d\\n\", cxx_sum(5)); return 0; }\n";
