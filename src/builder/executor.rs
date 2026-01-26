//! Build executor with progress reporting.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};

use crate::builder::context::BuildContext;
use crate::builder::native::NativeBuilder;
use crate::builder::plan::BuildPlan;
use crate::ops::harbour_build::Artifact;

/// Build executor with progress tracking.
pub struct BuildExecutor<'a> {
    ctx: &'a BuildContext,
    verbose: bool,
}

impl<'a> BuildExecutor<'a> {
    /// Create a new build executor.
    pub fn new(ctx: &'a BuildContext) -> Self {
        BuildExecutor {
            ctx,
            verbose: false,
        }
    }

    /// Enable verbose output.
    pub fn verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    /// Execute a build plan with progress reporting.
    pub fn execute(&self, plan: &BuildPlan, jobs: Option<usize>) -> Result<Vec<Artifact>> {
        let start = Instant::now();

        // Show build info
        if self.verbose {
            eprintln!("   Compiling {} file(s)", plan.compile_count());
            eprintln!("     Linking {} target(s)", plan.link_count());
        }

        // Create progress bar
        let total = plan.compile_count() + plan.link_count();
        let pb = if !self.verbose && total > 1 {
            let pb = ProgressBar::new(total as u64);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
                    .unwrap()
                    .progress_chars("#>-"),
            );
            Some(pb)
        } else {
            None
        };

        // Execute build
        let builder = NativeBuilder::new(self.ctx);
        let artifacts = builder.execute(plan, jobs)?;

        // Finish progress
        if let Some(pb) = pb {
            pb.finish_with_message("done");
        }

        let elapsed = start.elapsed();
        eprintln!(
            "    Finished {} target(s) in {:.2}s",
            artifacts.len(),
            elapsed.as_secs_f64()
        );

        Ok(artifacts)
    }
}

/// Progress callback for build steps.
#[derive(Clone)]
pub struct BuildProgress {
    compiled: Arc<AtomicUsize>,
    linked: Arc<AtomicUsize>,
    total_compile: usize,
    total_link: usize,
}

impl BuildProgress {
    /// Create a new progress tracker.
    pub fn new(total_compile: usize, total_link: usize) -> Self {
        BuildProgress {
            compiled: Arc::new(AtomicUsize::new(0)),
            linked: Arc::new(AtomicUsize::new(0)),
            total_compile,
            total_link,
        }
    }

    /// Record a completed compilation.
    pub fn compiled(&self) {
        self.compiled.fetch_add(1, Ordering::SeqCst);
    }

    /// Record a completed link.
    pub fn linked(&self) {
        self.linked.fetch_add(1, Ordering::SeqCst);
    }

    /// Get current compilation count.
    pub fn compile_count(&self) -> usize {
        self.compiled.load(Ordering::SeqCst)
    }

    /// Get current link count.
    pub fn link_count(&self) -> usize {
        self.linked.load(Ordering::SeqCst)
    }

    /// Get total progress as a fraction.
    pub fn progress(&self) -> f64 {
        let done = self.compile_count() + self.link_count();
        let total = self.total_compile + self.total_link;
        if total == 0 {
            1.0
        } else {
            done as f64 / total as f64
        }
    }

    /// Get total expected steps (compile + link).
    pub fn total(&self) -> usize {
        self.total_compile + self.total_link
    }

    /// Check if build is complete.
    pub fn is_complete(&self) -> bool {
        self.compile_count() >= self.total_compile && self.link_count() >= self.total_link
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_progress_new() {
        let progress = BuildProgress::new(10, 2);
        assert_eq!(progress.compile_count(), 0);
        assert_eq!(progress.link_count(), 0);
        assert_eq!(progress.total(), 12);
        assert!(!progress.is_complete());
    }

    #[test]
    fn test_build_progress_compiled() {
        let progress = BuildProgress::new(3, 1);
        assert_eq!(progress.compile_count(), 0);

        progress.compiled();
        assert_eq!(progress.compile_count(), 1);

        progress.compiled();
        progress.compiled();
        assert_eq!(progress.compile_count(), 3);
    }

    #[test]
    fn test_build_progress_linked() {
        let progress = BuildProgress::new(2, 3);
        assert_eq!(progress.link_count(), 0);

        progress.linked();
        assert_eq!(progress.link_count(), 1);

        progress.linked();
        progress.linked();
        assert_eq!(progress.link_count(), 3);
    }

    #[test]
    fn test_build_progress_progress_fraction() {
        let progress = BuildProgress::new(4, 1);
        assert_eq!(progress.progress(), 0.0);

        progress.compiled();
        assert!((progress.progress() - 0.2).abs() < 0.001);

        progress.compiled();
        progress.compiled();
        progress.compiled();
        assert!((progress.progress() - 0.8).abs() < 0.001);

        progress.linked();
        assert!((progress.progress() - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_build_progress_empty() {
        let progress = BuildProgress::new(0, 0);
        assert_eq!(progress.progress(), 1.0);
        assert!(progress.is_complete());
    }

    #[test]
    fn test_build_progress_is_complete() {
        let progress = BuildProgress::new(2, 1);
        assert!(!progress.is_complete());

        progress.compiled();
        progress.compiled();
        assert!(!progress.is_complete());

        progress.linked();
        assert!(progress.is_complete());
    }

    #[test]
    fn test_build_progress_thread_safe() {
        use std::thread;

        let progress = BuildProgress::new(100, 10);
        let progress_clone = progress.clone();

        let handles: Vec<_> = (0..10)
            .map(|_| {
                let p = progress.clone();
                thread::spawn(move || {
                    for _ in 0..10 {
                        p.compiled();
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(progress_clone.compile_count(), 100);
    }

    #[test]
    fn test_build_progress_clone() {
        let progress = BuildProgress::new(5, 2);
        let cloned = progress.clone();

        progress.compiled();
        progress.compiled();
        progress.linked();

        // Cloned instance shares the same atomic counters
        assert_eq!(cloned.compile_count(), 2);
        assert_eq!(cloned.link_count(), 1);
    }
}
