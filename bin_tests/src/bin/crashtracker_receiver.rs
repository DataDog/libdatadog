use datadog_crashtracker;

fn main() -> anyhow::Result<()> {
    datadog_crashtracker::receiver_entry_point()
}
