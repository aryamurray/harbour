//! `harbour doctor` command

use anyhow::Result;

use crate::cli::DoctorArgs;
use harbour::ops::{doctor, format_report, DoctorOptions};

pub fn execute(args: DoctorArgs, verbose: bool) -> Result<()> {
    let options = DoctorOptions {
        verbose: args.verbose || verbose,
        offline: args.offline,
    };

    let report = doctor(options)?;

    // Print the formatted report
    let output = format_report(&report, args.verbose || verbose);
    print!("{}", output);

    // Exit with error code if required checks failed
    if !report.all_required_passed() {
        std::process::exit(1);
    }

    Ok(())
}
