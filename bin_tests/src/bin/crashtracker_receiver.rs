use datadog_profiling::crashtracker;

fn main() -> anyhow::Result<()> {
    crashtracker::receiver_entry_point()
}
