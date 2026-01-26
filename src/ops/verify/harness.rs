//! Test harness generation and execution for verification.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::types::VerifyContext;
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
        // On Windows, use detected MSVC toolchain (supports auto-detection)
        use crate::builder::toolchain::detect_toolchain;

        let toolchain = detect_toolchain()
            .context("failed to detect MSVC toolchain for harness compilation")?;

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
        let compiler = if config.lang == "cxx" || config.lang == "c++" {
            std::env::var("CXX").unwrap_or_else(|_| "c++".to_string())
        } else {
            std::env::var("CC").unwrap_or_else(|_| "cc".to_string())
        };

        let mut cmd = Command::new(&compiler);
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
    let is_cross_compile = target_triple.map_or(false, |triple| {
        // Check if target triple differs from host
        let host_triple = get_host_triple();
        triple != host_triple
    });

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

/// Get the host target triple for the current platform.
pub(crate) fn get_host_triple() -> &'static str {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "x86_64-pc-windows-msvc"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86"))]
    {
        "i686-pc-windows-msvc"
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        "aarch64-pc-windows-msvc"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "aarch64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )))]
    {
        "unknown-unknown-unknown"
    }
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
