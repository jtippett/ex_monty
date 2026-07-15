use std::borrow::Cow;

use monty::{ExcType, MontyException, PrintWriterCallback, ResourceLimits};

/// Cumulative print-output budget for one run, preserved across interactive
/// snapshots. Monty's heap tracker does not account for `PrintWriter` buffers,
/// so the wrapper must apply the configured memory ceiling here as well.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct OutputBudget {
    max_bytes: Option<usize>,
    used_bytes: usize,
}

impl OutputBudget {
    pub fn from_limits(limits: &ResourceLimits) -> Self {
        Self {
            max_bytes: limits.max_memory,
            used_bytes: 0,
        }
    }

    pub fn collector(self) -> OutputCollector {
        OutputCollector {
            output: String::new(),
            budget: self,
        }
    }
}

/// Fallible collected-output writer. `String::push*` otherwise uses the global
/// allocator's infallible path, which can abort the whole VM under pressure.
pub struct OutputCollector {
    output: String,
    budget: OutputBudget,
}

impl OutputCollector {
    pub fn finish(mut self) -> (String, OutputBudget) {
        self.budget.used_bytes = self.budget.used_bytes.saturating_add(self.output.len());
        (self.output, self.budget)
    }

    fn reserve(&mut self, additional: usize) -> Result<(), MontyException> {
        let phase_total = self
            .output
            .len()
            .checked_add(additional)
            .ok_or_else(output_limit_error)?;
        let run_total = self
            .budget
            .used_bytes
            .checked_add(phase_total)
            .ok_or_else(output_limit_error)?;

        if self.budget.max_bytes.is_some_and(|max| run_total > max) {
            return Err(output_limit_error());
        }

        // `try_reserve_exact` keeps the backing capacity within the logical
        // budget instead of letting amortized growth double it past the ceiling.
        self.output
            .try_reserve_exact(additional)
            .map_err(|_| output_allocation_error())
    }
}

impl PrintWriterCallback for OutputCollector {
    fn stdout_write(&mut self, output: Cow<'_, str>) -> Result<(), MontyException> {
        self.reserve(output.len())?;
        self.output.push_str(&output);
        Ok(())
    }

    fn stdout_push(&mut self, end: char) -> Result<(), MontyException> {
        self.reserve(end.len_utf8())?;
        self.output.push(end);
        Ok(())
    }
}

fn output_limit_error() -> MontyException {
    MontyException::new(
        ExcType::MemoryError,
        Some("captured output exceeds the configured max_memory limit".to_string()),
    )
}

fn output_allocation_error() -> MontyException {
    MontyException::new(
        ExcType::MemoryError,
        Some("failed to allocate captured output".to_string()),
    )
}
