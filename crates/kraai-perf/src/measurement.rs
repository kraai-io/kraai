#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use supported::measure_async;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn measure_async<T: Send>(
    _action: impl std::future::Future<Output = color_eyre::Result<T>> + Send,
) -> std::future::Ready<color_eyre::Result<(T, crate::report::Sample)>> {
    std::future::ready(Err(color_eyre::eyre::eyre!(
        "performance measurement requires Linux or macOS"
    )))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod supported {
    use std::future::Future;
    use std::time::Instant;

    use color_eyre::eyre::{Result, WrapErr, eyre};
    use nix::sys::resource::{Usage, UsageWho, getrusage};
    use nix::sys::time::TimeValLike;

    use crate::report::Sample;

    pub async fn measure_async<T: Send>(
        action: impl Future<Output = Result<T>> + Send,
    ) -> Result<(T, Sample)> {
        let measurement = Measurement::start()?;
        let output = action.await?;
        let sample = measurement.finish()?;
        Ok((output, sample))
    }

    #[cfg(test)]
    pub(super) fn measure(action: impl FnOnce() -> Result<()>) -> Result<Sample> {
        let measurement = Measurement::start()?;
        action()?;
        measurement.finish()
    }

    struct Measurement {
        before: Usage,
        children_before: Usage,
        started: Instant,
    }

    impl Measurement {
        fn start() -> Result<Self> {
            let before = getrusage(UsageWho::RUSAGE_SELF).wrap_err("read initial process usage")?;
            let children_before = getrusage(UsageWho::RUSAGE_CHILDREN)
                .wrap_err("read initial child process usage")?;
            Ok(Self {
                before,
                children_before,
                started: Instant::now(),
            })
        }

        fn finish(self) -> Result<Sample> {
            let wall_ns = u64::try_from(self.started.elapsed().as_nanos())
                .wrap_err("wall time exceeds u64")?;
            let after = getrusage(UsageWho::RUSAGE_SELF).wrap_err("read final process usage")?;
            let children_after =
                getrusage(UsageWho::RUSAGE_CHILDREN).wrap_err("read final child process usage")?;

            Ok(Sample {
                wall_ns,
                user_cpu_ns: cpu_delta_ns(
                    self.before.user_time().num_microseconds(),
                    after.user_time().num_microseconds(),
                )
                .wrap_err("measure user CPU time")?,
                system_cpu_ns: cpu_delta_ns(
                    self.before.system_time().num_microseconds(),
                    after.system_time().num_microseconds(),
                )
                .wrap_err("measure system CPU time")?,
                peak_rss_bytes: peak_rss_bytes(&after)?,
                children_user_cpu_ns: cpu_delta_ns(
                    self.children_before.user_time().num_microseconds(),
                    children_after.user_time().num_microseconds(),
                )
                .wrap_err("measure child user CPU time")?,
                children_system_cpu_ns: cpu_delta_ns(
                    self.children_before.system_time().num_microseconds(),
                    children_after.system_time().num_microseconds(),
                )
                .wrap_err("measure child system CPU time")?,
                children_peak_rss_bytes: peak_rss_bytes(&children_after)?,
            })
        }
    }

    fn peak_rss_bytes(usage: &Usage) -> Result<u64> {
        let peak_rss = u64::try_from(usage.max_rss()).wrap_err("negative peak resident memory")?;

        #[cfg(target_os = "linux")]
        let peak_rss = peak_rss
            .checked_mul(1024)
            .ok_or_else(|| eyre!("peak resident memory exceeds u64 bytes"))?;

        Ok(peak_rss)
    }

    pub(super) fn cpu_delta_ns(before_us: i64, after_us: i64) -> Result<u64> {
        after_us
            .checked_sub(before_us)
            .and_then(|delta| u64::try_from(delta).ok())
            .and_then(|delta| delta.checked_mul(1000))
            .ok_or_else(|| eyre!("CPU counter decreased or exceeded u64 nanoseconds"))
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
#[path = "measurement_tests.rs"]
mod tests;
