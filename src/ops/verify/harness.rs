//! Test harness generation and execution for verification.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::types::VerifyContext;
use crate::core::target::TargetTriple;
use crate::sources::registry::shim::HarnessConfig;

/// Verify that artifacts exist.
pub(crate) fn verify_artifacts(artifacts: &[PathBuf]) -> Result<()> {
    for artifact in artifacts {
        if !artifact.exists() {
            bail!("artifact does not exist: {}", artifact.display());
        }

        let metadata = std::fs::metadata(artifact)?;
        if metadata.len() == 0 {
            bail!("artifact is empty: {}", artifact.display());
        }
    }

    Ok(())
}

/// Run the harness test.
pub(crate) fn run_harness_test(
    config: &HarnessConfig,
    ctx: &VerifyContext,
    source_dir: &Path,
    artifacts: &[PathBuf],
    target_triple: Option<&str>,
) -> Result<()> {
    // The harness must be compiled for the target under test, not for the
    // host. Previously the non-Windows path invoked $CC (or literally `cc`)
    // unconditionally, so verifying a cross target compiled the harness with
    // the host compiler and linked it against a cross-built archive.
    let target = target_triple.map(TargetTriple::parse);
    let harness_toolchain = crate::builder::toolchain::detect_toolchain(target.as_ref())
        .context("failed to detect a toolchain for harness compilation")?;

    // Create harness test directory
    let harness_dir = ctx.temp_dir.path().join("harness");
    std::fs::create_dir_all(&harness_dir)?;

    // Generate harness source file
    let harness_path = generate_harness(config, &harness_dir)?;

    // Find library and include paths
    let lib_path = artifacts
        .iter()
        .find(|p| {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            ext == "a" || ext == "lib"
        })
        .ok_or_else(|| anyhow::anyhow!("no library artifact found for harness test"))?;

    // Get include directory from surface override
    let include_dir = if let Some(surface) = ctx.shim.effective_surface_override() {
        surface
            .compile
            .as_ref()
            .and_then(|c| c.public.as_ref())
            .and_then(|p| p.include_dirs.first())
            .map(|d| source_dir.join(d))
            .unwrap_or_else(|| source_dir.to_path_buf())
    } else {
        source_dir.to_path_buf()
    };

    // Compile harness
    let output_path = harness_dir.join(if cfg!(windows) {
        "harness.exe"
    } else {
        "harness"
    });

    #[cfg(windows)]
    {
        let toolchain = &harness_toolchain;

        // Get compiler path and generate compile command
        let compiler = toolchain.compiler_path();

        let mut cmd = Command::new(compiler);
        cmd.arg("/nologo")
            .arg(format!("/I{}", include_dir.display()))
            .arg(&harness_path)
            .arg(lib_path)
            .arg(format!("/Fe:{}", output_path.display()));

        // Add C++ flag if needed
        if config.lang == "cxx" || config.lang == "c++" {
            cmd.arg("/TP"); // Treat source as C++
        }

        cmd.arg("/link");

        // Apply environment variables from toolchain (for auto-detected MSVC)
        // Generate a dummy compile command to get the environment
        let dummy_input = crate::builder::toolchain::CompileInput {
            source: harness_path.clone(),
            output: output_path.clone(),
            include_dirs: vec![],
            defines: vec![],
            cflags: vec![],
        };
        let spec = toolchain.compile_command(&dummy_input, crate::core::target::Language::C, None);
        for (key, value) in &spec.env {
            cmd.env(key, value);
        }

        tracing::debug!("Running harness compile: {:?}", cmd);

        let output = cmd.output().context("failed to compile harness")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            bail!("harness compilation failed:\n{}\n{}", stdout, stderr);
        }
    }

    #[cfg(not(windows))]
    {
        // Sourced from the resolved toolchain rather than $CC/$CXX, so a
        // cross target gets its cross compiler. detect_toolchain still
        // honours CC/CXX for host builds, so host behaviour is unchanged.
        let compiler = if config.lang == "cxx" || config.lang == "c++" {
            harness_toolchain.cxx_compiler_path()
        } else {
            harness_toolchain.compiler_path()
        };

        let mut cmd = Command::new(compiler);
        cmd.arg(&harness_path)
            .arg("-o")
            .arg(&output_path)
            .arg(format!("-I{}", include_dir.display()))
            .arg(lib_path);

        // Add platform-specific link flags
        #[cfg(target_os = "linux")]
        {
            cmd.args(["-lpthread", "-ldl", "-lm"]);
        }

        #[cfg(target_os = "macos")]
        {
            // Common macOS frameworks that C libraries may depend on
            cmd.args(["-framework", "CoreFoundation"]);
        }

        tracing::debug!("Running harness compile: {:?}", cmd);

        let output = cmd.output().context("failed to compile harness")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            bail!("harness compilation failed:\n{}\n{}", stdout, stderr);
        }
    }

    // Run harness (skip for cross-compilation)
    let is_cross_compile =
        target_triple.is_some_and(|triple| !TargetTriple::parse(triple).is_host());

    if is_cross_compile {
        tracing::info!("Harness compiled successfully (execution skipped for cross-compilation)");
        return Ok(());
    }

    // Execute the harness
    tracing::info!("Running harness test");
    let output = Command::new(&output_path)
        .output()
        .context("failed to execute harness")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "harness execution failed (exit code {:?}):\n{}\n{}",
            output.status.code(),
            stdout,
            stderr
        );
    }

    tracing::info!("Harness test passed");
    Ok(())
}

/// Generate a C test harness file.
pub fn generate_harness_c(config: &HarnessConfig, output_path: &Path) -> Result<()> {
    let content = format!(
        r#"/* Auto-generated harness test for Harbour */
#include <{header}>

int main(void) {{
    {test_call};
    return 0;
}}
"#,
        header = config.header,
        test_call = config.test_call
    );

    std::fs::write(output_path, content)
        .with_context(|| format!("failed to write harness file: {}", output_path.display()))?;

    Ok(())
}

/// Generate a C++ test harness file.
pub fn generate_harness_cxx(config: &HarnessConfig, output_path: &Path) -> Result<()> {
    let content = format!(
        r#"// Auto-generated harness test for Harbour
#include <{header}>

int main() {{
    {test_call};
    return 0;
}}
"#,
        header = config.header,
        test_call = config.test_call
    );

    std::fs::write(output_path, content)
        .with_context(|| format!("failed to write harness file: {}", output_path.display()))?;

    Ok(())
}

/// Generate the appropriate harness based on language.
pub fn generate_harness(config: &HarnessConfig, output_dir: &Path) -> Result<PathBuf> {
    // The fn-pointer type is inherently a bit noisy to clippy; introducing a `type` alias
    // here would only be used at this single call site, so it's not worth the indirection.
    #[allow(clippy::type_complexity)]
    let (filename, generator): (&str, fn(&HarnessConfig, &Path) -> Result<()>) =
        if config.lang == "cxx" || config.lang == "c++" {
            ("harness_test.cpp", generate_harness_cxx)
        } else {
            ("harness_test.c", generate_harness_c)
        };

    let output_path = output_dir.join(filename);
    generator(config, &output_path)?;
    Ok(output_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_harness_c() {
        let config = HarnessConfig {
            header: "zlib.h".to_string(),
            test_call: "zlibVersion()".to_string(),
            lang: "c".to_string(),
        };

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.c");
        generate_harness_c(&config, &path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("#include <zlib.h>"));
        assert!(content.contains("zlibVersion()"));
    }

    #[test]
    fn test_generate_harness_cxx() {
        let config = HarnessConfig {
            header: "fmt/core.h".to_string(),
            test_call: "fmt::format(\"test\")".to_string(),
            lang: "cxx".to_string(),
        };

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.cpp");
        generate_harness_cxx(&config, &path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("#include <fmt/core.h>"));
        assert!(content.contains("fmt::format"));
    }
}
