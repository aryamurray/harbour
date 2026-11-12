//! A dependent linking several library targets of one package
//! (`targets = [...]` in a per-target `deps` entry), and the sibling closure
//! and link order that make that safe.
#![allow(unused_imports)]

use std::fs;
use std::path::{Path, PathBuf};

use crate::common::*;

/// `libssl.a` / `ssl.lib`, whichever the toolchain produces.
fn archive_name(lib: &str) -> String {
    if cfg!(windows) {
        format!("{lib}.lib")
    } else {
        format!("lib{lib}.a")
    }
}

/// A package `ossl` shaped like openssl: `ssl` needs `crypto`, and says so
/// with `local = true`.
///
/// `crypto` is split over two translation units on purpose. `crypto_value`
/// (`crypto_a.c`) is called by the *program*; `crypto_secret` (`crypto_b.c`)
/// is called only by `ssl`. With `libcrypto.a` ahead of `libssl.a` on the
/// link line a single-pass linker has already decided not to pull
/// `crypto_b.o` by the time it reads `libssl.a`, and the link fails on
/// `crypto_secret` -- so a wrong order is a test failure, not a lucky pass.
fn write_ossl(dir: &Path) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::create_dir_all(dir.join("include/ssl")).unwrap();
    fs::create_dir_all(dir.join("include/crypto")).unwrap();
    fs::write(
        dir.join("include/crypto/crypto.h"),
        "int crypto_value(void);\nint crypto_secret(void);\n",
    )
    .unwrap();
    fs::write(dir.join("include/ssl/ssl.h"), "int ssl_value(void);\n").unwrap();
    fs::write(
        dir.join("src/crypto_a.c"),
        "int crypto_value(void) { return 1; }\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/crypto_b.c"),
        "int crypto_secret(void) { return 40; }\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/ssl.c"),
        "#include <crypto.h>\nint ssl_value(void) { return crypto_secret() + 1; }\n",
    )
    .unwrap();
    // `ssl` is declared first, so it is also the default target.
    fs::write(
        dir.join("Harbour.toml"),
        "[package]\nname = \"ossl\"\nversion = \"0.1.0\"\n\n\
         [targets.ssl]\nkind = \"staticlib\"\nsources = [\"src/ssl.c\"]\n\n\
         [targets.ssl.surface.compile.public]\ninclude_dirs = [\"include/ssl\"]\n\n\
         [targets.ssl.deps]\ncrypto = { local = true }\n\n\
         [targets.crypto]\nkind = \"staticlib\"\n\
         sources = [\"src/crypto_a.c\", \"src/crypto_b.c\"]\n\n\
         [targets.crypto.surface.compile.public]\ninclude_dirs = [\"include/crypto\"]\n",
    )
    .unwrap();
}

/// A dependent of the package at `../<dep>`: `main_c` is its program,
/// `dep_entry` the right-hand side of its `[targets.app.deps]` line.
fn write_app(dir: &Path, dep: &str, dep_entry: &str, main_c: &str) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/main.c"), main_c).unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\n{dep} = {{ path = \"../{dep}\" }}\n\n\
             [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n\n\
             [targets.app.deps]\n{dep} = {dep_entry}\n"
        ),
    )
    .unwrap();
}

const OSSL_MAIN: &str = "#include <stdio.h>\n#include <ssl.h>\n#include <crypto.h>\n\
     int main(void) { printf(\"%d\\n\", ssl_value() + crypto_value()); return 0; }\n";

/// What a consumer that names only `ssl` gets, with the sibling edge
/// `ssl -> crypto` and nothing else: the closure is followed for a package
/// dependency's chosen target, so this passes without `targets`.
#[test]
fn test_naming_one_target_still_links_its_sibling_closure() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_ossl(&tmp.path().join("ossl"));
    let app = tmp.path().join("app");
    write_app(&app, "ossl", "{ target = \"ssl\" }", OSSL_MAIN);

    build_ok(&home, &app);
    assert_eq!(run_built_exe(&app, "app").out(), "42");
}

/// `targets = [...]` links every named library, in dependents-first order
/// whatever order the manifest wrote them in.
#[test]
fn test_targets_list_links_every_named_library() {
    for list in ["[\"ssl\", \"crypto\"]", "[\"crypto\", \"ssl\"]"] {
        let tmp = temp_dir();
        let home = harbour_home(&tmp);
        write_ossl(&tmp.path().join("ossl"));
        let app = tmp.path().join("app");
        write_app(&app, "ossl", &format!("{{ targets = {list} }}"), OSSL_MAIN);

        build_ok(&home, &app);
        assert_eq!(
            run_built_exe(&app, "app").out(),
            "42",
            "`targets = {list}` must build and run"
        );
    }
}

/// Two libraries with no edge between them.
fn write_ab(dir: &Path) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::create_dir_all(dir.join("include")).unwrap();
    fs::write(
        dir.join("include/ab.h"),
        "int a_value(void);\nint b_value(void);\n",
    )
    .unwrap();
    fs::write(dir.join("src/a.c"), "int a_value(void) { return 20; }\n").unwrap();
    fs::write(dir.join("src/b.c"), "int b_value(void) { return 22; }\n").unwrap();
    fs::write(
        dir.join("Harbour.toml"),
        "[package]\nname = \"ab\"\nversion = \"0.1.0\"\n\n\
         [targets.a]\nkind = \"staticlib\"\nsources = [\"src/a.c\"]\n\n\
         [targets.a.surface.compile.public]\ninclude_dirs = [\"include\"]\n\n\
         [targets.b]\nkind = \"staticlib\"\nsources = [\"src/b.c\"]\n\n\
         [targets.b.surface.compile.public]\ninclude_dirs = [\"include\"]\n",
    )
    .unwrap();
}

const AB_MAIN: &str = "#include <stdio.h>\n#include <ab.h>\n\
     int main(void) { printf(\"%d\\n\", a_value() + b_value()); return 0; }\n";

#[test]
fn test_two_unrelated_libraries_are_both_linked() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_ab(&tmp.path().join("ab"));
    let app = tmp.path().join("app");
    write_app(&app, "ab", "{ targets = [\"a\", \"b\"] }", AB_MAIN);

    build_ok(&home, &app);
    assert_eq!(run_built_exe(&app, "app").out(), "42");
}

/// Each malformed spelling is refused, and the message says what is wrong.
#[test]
fn test_malformed_targets_entries_are_refused() {
    let cases: [(&str, &[&str]); 4] = [
        (
            "{ target = \"a\", targets = [\"a\", \"b\"] }",
            &["target", "targets", "both"],
        ),
        ("{ targets = [] }", &["targets", "empty"]),
        ("{ targets = [\"a\", \"a\"] }", &["targets", "twice", "`a`"]),
        ("{ targets = [\"a\", \"nope\"] }", &["nope", "available"]),
    ];
    for (entry, needles) in cases {
        let tmp = temp_dir();
        let home = harbour_home(&tmp);
        write_ab(&tmp.path().join("ab"));
        let app = tmp.path().join("app");
        write_app(&app, "ab", entry, AB_MAIN);

        let run = harbour_run(&home, &app, &["build"]);
        assert!(!run.status.success(), "`{entry}` must be refused\n{run}");
        let text = run.to_string();
        for needle in needles {
            assert!(
                text.contains(needle),
                "the error for `{entry}` must mention `{needle}`\n{text}"
            );
        }
    }
}

/// `harbour linkplan` reads the same fold as the build, and shows `ssl`
/// ahead of `crypto` even though the manifest listed them the other way.
#[test]
fn test_linkplan_orders_the_archives_dependents_first() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_ossl(&tmp.path().join("ossl"));
    let app = tmp.path().join("app");
    write_app(
        &app,
        "ossl",
        "{ targets = [\"crypto\", \"ssl\"] }",
        OSSL_MAIN,
    );

    let plan = harbour_run(&home, &app, &["linkplan", "app"]).success();
    let (_, link_line) = plan
        .stdout
        .split_once("Link line (what the linker receives")
        .unwrap_or_else(|| panic!("linkplan printed no link line section\n{plan}"));
    let ssl = link_line.find(&archive_name("ssl"));
    let crypto = link_line.find(&archive_name("crypto"));
    assert!(
        matches!((ssl, crypto), (Some(s), Some(c)) if s < c),
        "the link line must list ssl before crypto\n{plan}"
    );
    for lib in ["ssl", "crypto"] {
        assert_eq!(
            link_line.matches(&archive_name(lib)).count(),
            1,
            "each archive appears once\n{plan}"
        );
    }
}

/// Adding a name to `targets` changes what is linked, so the program is
/// relinked: the set of archives is part of the link fingerprint.
#[test]
fn test_changing_targets_relinks_the_dependent() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_ab(&tmp.path().join("ab"));
    let app = tmp.path().join("app");
    let only_a = "#include <stdio.h>\n#include <ab.h>\n\
         int main(void) { printf(\"%d\\n\", a_value() + 22); return 0; }\n";
    write_app(&app, "ab", "{ targets = [\"a\"] }", only_a);
    build_ok(&home, &app);
    assert_eq!(run_built_exe(&app, "app").out(), "42");

    // Same sources, one more archive.
    let manifest = fs::read_to_string(app.join("Harbour.toml")).unwrap();
    fs::write(
        app.join("Harbour.toml"),
        manifest.replace("targets = [\"a\"]", "targets = [\"a\", \"b\"]"),
    )
    .unwrap();
    let (log, diff) = rebuild_and_diff(&home, &app);
    diff.assert_touched(
        "bin/",
        "a name was added to `targets`, so the program links one more archive and \
         must be relinked",
    );
    assert_eq!(run_built_exe(&app, "app").out(), "42", "{log}\n{diff}");

    // And removing it relinks again.
    fs::write(app.join("Harbour.toml"), manifest).unwrap();
    let (_, diff) = rebuild_and_diff(&home, &app);
    diff.assert_touched(
        "bin/",
        "a name was removed from `targets`, so the program must be relinked",
    );
}

/// `app -> mid -> ossl(targets = [ssl, crypto])`, all static: the final link
/// carries both archives although `app` never names `ossl`.
#[test]
fn test_a_transitive_consumer_links_every_archive_its_dependency_selected() {
    let tmp = temp_dir();
    let home = harbour_home(&tmp);
    write_ossl(&tmp.path().join("ossl"));
    // The package's default is `crypto` alone, so an `app` that fell back to
    // the default for a package it does not name itself would link no `ssl`
    // and fail: only `mid`'s selection can put `libssl.a` on `app`'s line.
    let ossl_manifest = tmp.path().join("ossl/Harbour.toml");
    let text = fs::read_to_string(&ossl_manifest).unwrap();
    fs::write(
        &ossl_manifest,
        text.replace(
            "version = \"0.1.0\"\n",
            "version = \"0.1.0\"\ndefault_target = \"crypto\"\n",
        ),
    )
    .unwrap();

    let mid = tmp.path().join("mid");
    fs::create_dir_all(mid.join("src")).unwrap();
    fs::create_dir_all(mid.join("include")).unwrap();
    fs::write(mid.join("include/mid.h"), "int mid_value(void);\n").unwrap();
    fs::write(
        mid.join("src/mid.c"),
        "#include <ssl.h>\n#include <crypto.h>\n\
         int mid_value(void) { return ssl_value() + crypto_value(); }\n",
    )
    .unwrap();
    fs::write(
        mid.join("Harbour.toml"),
        "[package]\nname = \"mid\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nossl = { path = \"../ossl\" }\n\n\
         [targets.mid]\nkind = \"staticlib\"\nsources = [\"src/mid.c\"]\n\n\
         [targets.mid.surface.compile.public]\ninclude_dirs = [\"include\"]\n\n\
         [targets.mid.deps]\nossl = { targets = [\"ssl\", \"crypto\"] }\n",
    )
    .unwrap();

    let app = tmp.path().join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("src/main.c"),
        "#include <stdio.h>\n#include <mid.h>\n\
         int main(void) { printf(\"%d\\n\", mid_value()); return 0; }\n",
    )
    .unwrap();
    fs::write(
        app.join("Harbour.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nmid = { path = \"../mid\" }\n\n\
         [targets.app]\nkind = \"exe\"\nsources = [\"src/main.c\"]\n",
    )
    .unwrap();

    build_ok(&home, &app);
    assert_eq!(run_built_exe(&app, "app").out(), "42");
}
