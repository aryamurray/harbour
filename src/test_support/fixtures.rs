//! Test fixtures for common test scenarios.
//!
//! This module provides pre-built test data and fixture generators
//! for common testing patterns in Harbour.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Fixture for a complete project structure.
#[derive(Debug, Clone)]
pub struct ProjectFixture {
    /// Project name.
    pub name: String,
    /// Harbor.toml content.
    pub manifest: String,
    /// Source files (path relative to project root -> content).
    pub sources: HashMap<PathBuf, String>,
    /// Header files (path relative to project root -> content).
    pub headers: HashMap<PathBuf, String>,
    /// Dependencies (other ProjectFixtures).
    pub dependencies: Vec<ProjectFixture>,
}

impl ProjectFixture {
    /// Create a new empty project fixture.
    pub fn new(name: impl Into<String>) -> Self {
        ProjectFixture {
            name: name.into(),
            manifest: String::new(),
            sources: HashMap::new(),
            headers: HashMap::new(),
            dependencies: Vec::new(),
        }
    }

    /// Create a minimal library project fixture.
    pub fn library(name: impl Into<String>) -> Self {
        let name = name.into();
        let manifest = format!(
            r#"[package]
name = "{name}"
version = "1.0.0"

[targets.{name}]
kind = "staticlib"
sources = ["src/**/*.c"]
public_headers = ["include/**/*.h"]

[targets.{name}.public]
include_dirs = ["include"]
"#
        );

        let mut sources = HashMap::new();
        sources.insert(
            PathBuf::from("src/lib.c"),
            format!(
                r#"#include "{name}.h"

int {name}_init(void) {{
    return 0;
}}
"#
            ),
        );

        let mut headers = HashMap::new();
        let guard = name.to_uppercase().replace('-', "_");
        headers.insert(
            PathBuf::from(format!("include/{}.h", name)),
            format!(
                r#"#ifndef {guard}_H
#define {guard}_H

int {name}_init(void);

#endif // {guard}_H
"#
            ),
        );

        ProjectFixture {
            name,
            manifest,
            sources,
            headers,
            dependencies: Vec::new(),
        }
    }

    /// Create a minimal executable project fixture.
    pub fn executable(name: impl Into<String>) -> Self {
        let name = name.into();
        let manifest = format!(
            r#"[package]
name = "{name}"
version = "1.0.0"

[targets.{name}]
kind = "exe"
sources = ["src/**/*.c"]
"#
        );

        let mut sources = HashMap::new();
        sources.insert(
            PathBuf::from("src/main.c"),
            r#"#include <stdio.h>

int main(void) {
    printf("Hello, World!\n");
    return 0;
}
"#
            .to_string(),
        );

        ProjectFixture {
            name,
            manifest,
            sources,
            headers: HashMap::new(),
            dependencies: Vec::new(),
        }
    }

    /// Create a header-only library fixture.
    pub fn header_only(name: impl Into<String>) -> Self {
        let name = name.into();
        let manifest = format!(
            r#"[package]
name = "{name}"
version = "1.0.0"

[targets.{name}]
kind = "header-only"
public_headers = ["include/**/*.h"]

[targets.{name}.public]
include_dirs = ["include"]
"#
        );

        let guard = name.to_uppercase().replace('-', "_");
        let mut headers = HashMap::new();
        headers.insert(
            PathBuf::from(format!("include/{}.h", name)),
            format!(
                r#"#ifndef {guard}_H
#define {guard}_H

// Header-only library: {name}

static inline int {name}_inline_func(void) {{
    return 42;
}}

#endif // {guard}_H
"#
            ),
        );

        ProjectFixture {
            name,
            manifest,
            sources: HashMap::new(),
            headers,
            dependencies: Vec::new(),
        }
    }

    /// Set the manifest content.
    pub fn with_manifest(mut self, manifest: impl Into<String>) -> Self {
        self.manifest = manifest.into();
        self
    }

    /// Add a source file.
    pub fn with_source(mut self, path: impl Into<PathBuf>, content: impl Into<String>) -> Self {
        self.sources.insert(path.into(), content.into());
        self
    }

    /// Add a header file.
    pub fn with_header(mut self, path: impl Into<PathBuf>, content: impl Into<String>) -> Self {
        self.headers.insert(path.into(), content.into());
        self
    }

    /// Add a dependency.
    pub fn with_dependency(mut self, dep: ProjectFixture) -> Self {
        self.dependencies.push(dep);
        self
    }

    /// Write this fixture to a real directory.
    pub fn write_to(&self, base_path: &Path) -> std::io::Result<PathBuf> {
        let project_path = base_path.join(&self.name);
        std::fs::create_dir_all(&project_path)?;

        // Write manifest
        std::fs::write(project_path.join("Harbor.toml"), &self.manifest)?;

        // Write sources
        for (rel_path, content) in &self.sources {
            let full_path = project_path.join(rel_path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full_path, content)?;
        }

        // Write headers
        for (rel_path, content) in &self.headers {
            let full_path = project_path.join(rel_path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full_path, content)?;
        }

        // Write dependencies
        for dep in &self.dependencies {
            dep.write_to(base_path)?;
        }

        Ok(project_path)
    }

    /// Write this fixture to a MockFileSystem.
    pub fn write_to_mock(&self, fs: &mut super::MockFileSystem, base_path: &Path) {
        let project_path = base_path.join(&self.name);
        fs.add_dir(&project_path);

        // Write manifest
        fs.add_file(project_path.join("Harbor.toml"), self.manifest.as_bytes());

        // Write sources
        for (rel_path, content) in &self.sources {
            let full_path = project_path.join(rel_path);
            fs.add_file(&full_path, content.as_bytes());
        }

        // Write headers
        for (rel_path, content) in &self.headers {
            let full_path = project_path.join(rel_path);
            fs.add_file(&full_path, content.as_bytes());
        }

        // Write dependencies
        for dep in &self.dependencies {
            dep.write_to_mock(fs, base_path);
        }
    }
}

/// Common manifest templates.
pub mod manifests {
    /// A workspace manifest with members.
    pub fn workspace(members: &[&str]) -> String {
        let members_str = members
            .iter()
            .map(|m| format!("\"{}\"", m))
            .collect::<Vec<_>>()
            .join(", ");

        format!(
            r#"[workspace]
members = [{members_str}]
"#
        )
    }

    /// A workspace manifest with shared dependencies.
    pub fn workspace_with_deps(members: &[&str], deps: &[(&str, &str)]) -> String {
        let members_str = members
            .iter()
            .map(|m| format!("\"{}\"", m))
            .collect::<Vec<_>>()
            .join(", ");

        let mut manifest = format!(
            r#"[workspace]
members = [{members_str}]

[workspace.dependencies]
"#
        );

        for (name, spec) in deps {
            manifest.push_str(&format!("{} = {}\n", name, spec));
        }

        manifest
    }

    /// A library manifest with a C++ target.
    pub fn cpp_library(name: &str, cpp_std: &str) -> String {
        format!(
            r#"[package]
name = "{name}"
version = "1.0.0"

[targets.{name}]
kind = "staticlib"
lang = "c++"
cpp_std = "{cpp_std}"
sources = ["src/**/*.cpp"]
public_headers = ["include/**/*.hpp"]

[targets.{name}.public]
include_dirs = ["include"]
"#
        )
    }

    /// A library with system dependencies.
    pub fn library_with_system_deps(name: &str, system_libs: &[&str]) -> String {
        let libs_str = system_libs
            .iter()
            .map(|l| format!("\"{}\"", l))
            .collect::<Vec<_>>()
            .join(", ");

        format!(
            r#"[package]
name = "{name}"
version = "1.0.0"

[targets.{name}]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.{name}.public]
system_libs = [{libs_str}]
"#
        )
    }

    /// A library with defines.
    pub fn library_with_defines(name: &str, defines: &[(&str, Option<&str>)]) -> String {
        let defines_str = defines
            .iter()
            .map(|(k, v)| match v {
                Some(val) => format!("\"{}={}\"", k, val),
                None => format!("\"{}\"", k),
            })
            .collect::<Vec<_>>()
            .join(", ");

        format!(
            r#"[package]
name = "{name}"
version = "1.0.0"

[targets.{name}]
kind = "staticlib"
sources = ["src/**/*.c"]

[targets.{name}.public]
defines = [{defines_str}]
"#
        )
    }

    /// A manifest with custom build profiles.
    pub fn with_profiles(name: &str, debug_opt: &str, release_opt: &str) -> String {
        format!(
            r#"[package]
name = "{name}"
version = "1.0.0"

[targets.{name}]
kind = "staticlib"
sources = ["src/**/*.c"]

[profile.debug]
opt_level = "{debug_opt}"
debug = "2"

[profile.release]
opt_level = "{release_opt}"
lto = true
"#
        )
    }
}

/// Common source file templates.
pub mod sources {
    /// A simple C library source file.
    pub fn simple_lib(name: &str) -> String {
        format!(
            r#"// {name} library implementation

#include "{name}.h"

static int initialized = 0;

int {name}_init(void) {{
    if (initialized) {{
        return 1;
    }}
    initialized = 1;
    return 0;
}}

void {name}_cleanup(void) {{
    initialized = 0;
}}
"#
        )
    }

    /// A simple C++ library source file.
    pub fn simple_cpp_lib(name: &str, namespace: &str) -> String {
        format!(
            r#"// {name} C++ library implementation

#include "{name}.hpp"

namespace {namespace} {{

class {name}::Impl {{
public:
    Impl() = default;
    ~Impl() = default;
}};

{name}::{name}() : pimpl(std::make_unique<Impl>()) {{}}
{name}::~{name}() = default;

}} // namespace {namespace}
"#
        )
    }

    /// A test harness main file.
    pub fn test_main(tests: &[&str]) -> String {
        let test_calls = tests
            .iter()
            .map(|t| format!("    run_test(\"{t}\", test_{t});"))
            .collect::<Vec<_>>()
            .join("\n");

        let test_decls = tests
            .iter()
            .map(|t| format!("extern int test_{t}(void);"))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            r#"#include <stdio.h>

// Test declarations
{test_decls}

static int passed = 0;
static int failed = 0;

static void run_test(const char* name, int (*test)(void)) {{
    printf("Running %s... ", name);
    if (test() == 0) {{
        printf("PASSED\n");
        passed++;
    }} else {{
        printf("FAILED\n");
        failed++;
    }}
}}

int main(void) {{
{test_calls}

    printf("\n%d passed, %d failed\n", passed, failed);
    return failed > 0 ? 1 : 0;
}}
"#
        )
    }

    /// A source file with syntax errors (for testing error handling).
    pub fn invalid_c() -> &'static str {
        r#"// This file has intentional syntax errors
int main(void) {
    int x = ;  // Missing value
    return
}  // Missing semicolon
"#
    }

    /// An empty source file.
    pub fn empty() -> &'static str {
        "// Empty source file\n"
    }
}

/// Common header file templates.
pub mod headers {
    /// A simple C library header.
    pub fn simple_lib(name: &str) -> String {
        let guard = name.to_uppercase().replace('-', "_");
        format!(
            r#"#ifndef {guard}_H
#define {guard}_H

#ifdef __cplusplus
extern "C" {{
#endif

/**
 * Initialize the {name} library.
 * @return 0 on success, non-zero on failure.
 */
int {name}_init(void);

/**
 * Clean up the {name} library.
 */
void {name}_cleanup(void);

#ifdef __cplusplus
}}
#endif

#endif // {guard}_H
"#
        )
    }

    /// A C++ library header.
    pub fn simple_cpp_lib(name: &str, namespace: &str) -> String {
        let guard = name.to_uppercase().replace('-', "_");
        format!(
            r#"#ifndef {guard}_HPP
#define {guard}_HPP

#include <memory>

namespace {namespace} {{

/**
 * @brief Main class for {name}.
 */
class {name} {{
public:
    {name}();
    ~{name}();

    // Non-copyable
    {name}(const {name}&) = delete;
    {name}& operator=(const {name}&) = delete;

    // Movable
    {name}({name}&&) noexcept = default;
    {name}& operator=({name}&&) noexcept = default;

private:
    class Impl;
    std::unique_ptr<Impl> pimpl;
}};

}} // namespace {namespace}

#endif // {guard}_HPP
"#
        )
    }

    /// A header with various preprocessor directives.
    pub fn with_conditionals(name: &str) -> String {
        let guard = name.to_uppercase().replace('-', "_");
        format!(
            r#"#ifndef {guard}_H
#define {guard}_H

// Platform detection
#if defined(_WIN32) || defined(_WIN64)
    #define {guard}_PLATFORM_WINDOWS 1
#elif defined(__APPLE__)
    #define {guard}_PLATFORM_MACOS 1
#elif defined(__linux__)
    #define {guard}_PLATFORM_LINUX 1
#else
    #error "Unsupported platform"
#endif

// Export macros
#ifdef {guard}_BUILD_SHARED
    #ifdef {guard}_PLATFORM_WINDOWS
        #define {guard}_API __declspec(dllexport)
    #else
        #define {guard}_API __attribute__((visibility("default")))
    #endif
#else
    #define {guard}_API
#endif

{guard}_API int {name}_init(void);

#endif // {guard}_H
"#
        )
    }
}

/// Mock compiler outputs for testing build processes.
pub mod compiler_outputs {
    use super::super::MockProcessOutput;

    /// GCC version output.
    pub fn gcc_version(version: &str) -> MockProcessOutput {
        MockProcessOutput::success(format!(
            "gcc (GCC) {version}\nCopyright (C) 2024 Free Software Foundation, Inc."
        ))
    }

    /// Clang version output.
    pub fn clang_version(version: &str) -> MockProcessOutput {
        MockProcessOutput::success(format!(
            "clang version {version}\nTarget: x86_64-unknown-linux-gnu"
        ))
    }

    /// MSVC version output.
    pub fn msvc_version() -> MockProcessOutput {
        MockProcessOutput::success(
            "Microsoft (R) C/C++ Optimizing Compiler Version 19.36.32532 for x64",
        )
    }

    /// Successful compilation.
    pub fn compile_success() -> MockProcessOutput {
        MockProcessOutput::success("")
    }

    /// Compilation with warnings.
    pub fn compile_with_warnings(warnings: &[&str]) -> MockProcessOutput {
        let stderr = warnings.join("\n");
        MockProcessOutput::with_output(0, "", stderr)
    }

    /// Compilation failure.
    pub fn compile_error(file: &str, line: u32, message: &str) -> MockProcessOutput {
        MockProcessOutput::failure(1, format!("{}:{}: error: {}", file, line, message))
    }

    /// Successful linking.
    pub fn link_success() -> MockProcessOutput {
        MockProcessOutput::success("")
    }

    /// Link failure with undefined symbol.
    pub fn link_undefined_symbol(symbol: &str) -> MockProcessOutput {
        MockProcessOutput::failure(1, format!("undefined reference to `{}'", symbol))
    }

    /// CMake configuration output.
    pub fn cmake_configure_success() -> MockProcessOutput {
        MockProcessOutput::success(
            "-- Configuring done\n-- Generating done\n-- Build files have been written to: /build",
        )
    }

    /// Git clone output.
    pub fn git_clone_success(url: &str) -> MockProcessOutput {
        MockProcessOutput::success(format!("Cloning into '{}'...\ndone.", url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_fixture_library() {
        let fixture = ProjectFixture::library("mylib");

        assert_eq!(fixture.name, "mylib");
        assert!(fixture.manifest.contains("name = \"mylib\""));
        assert!(fixture.manifest.contains("kind = \"staticlib\""));
        assert!(fixture.sources.contains_key(&PathBuf::from("src/lib.c")));
        assert!(fixture
            .headers
            .contains_key(&PathBuf::from("include/mylib.h")));
    }

    #[test]
    fn test_project_fixture_executable() {
        let fixture = ProjectFixture::executable("myapp");

        assert_eq!(fixture.name, "myapp");
        assert!(fixture.manifest.contains("kind = \"exe\""));
        assert!(fixture.sources.contains_key(&PathBuf::from("src/main.c")));
    }

    #[test]
    fn test_project_fixture_write_to_mock() {
        let mut fs = super::super::MockFileSystem::new();
        let fixture = ProjectFixture::library("testlib");

        fixture.write_to_mock(&mut fs, Path::new("/projects"));

        assert!(fs.exists(Path::new("/projects/testlib")));
        assert!(fs.exists(Path::new("/projects/testlib/Harbor.toml")));
        assert!(fs.exists(Path::new("/projects/testlib/src/lib.c")));
        assert!(fs.exists(Path::new("/projects/testlib/include/testlib.h")));
    }

    #[test]
    fn test_manifest_templates() {
        let ws = manifests::workspace(&["pkg1", "pkg2"]);
        assert!(ws.contains("members = [\"pkg1\", \"pkg2\"]"));

        let cpp = manifests::cpp_library("cpplib", "17");
        assert!(cpp.contains("lang = \"c++\""));
        assert!(cpp.contains("cpp_std = \"17\""));

        let syslib = manifests::library_with_system_deps("mylib", &["pthread", "m"]);
        assert!(syslib.contains("system_libs = [\"pthread\", \"m\"]"));
    }

    #[test]
    fn test_source_templates() {
        let lib = sources::simple_lib("mylib");
        assert!(lib.contains("#include \"mylib.h\""));
        assert!(lib.contains("int mylib_init(void)"));

        let test_main = sources::test_main(&["foo", "bar"]);
        assert!(test_main.contains("extern int test_foo(void);"));
        assert!(test_main.contains("run_test(\"foo\", test_foo);"));
    }

    #[test]
    fn test_header_templates() {
        let header = headers::simple_lib("mylib");
        assert!(header.contains("#ifndef MYLIB_H"));
        assert!(header.contains("int mylib_init(void);"));

        let cpp_header = headers::simple_cpp_lib("MyClass", "myns");
        assert!(cpp_header.contains("namespace myns"));
        assert!(cpp_header.contains("class MyClass"));
    }
}
