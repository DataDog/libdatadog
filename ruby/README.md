# libdatadog Ruby gem

`libdatadog` provides a shared library containing common code used in the implementation of Datadog's libraries,
including [Continuous Profilers](https://docs.datadoghq.com/tracing/profiler/).

(In a past life, `libdatadog` was known as [`libddprof`](https://github.com/datadog/libddprof) but it was renamed when
we decided to increase its scope).

**NOTE**: If you're building a new Datadog library/profiler or want to contribute to Datadog's existing tools, you've come to the
right place!
Otherwise, this is possibly not the droid you were looking for.

## Development

Run `bundle exec rake` to run the tests and the style autofixer.
You can also run `bundle exec pry` for an interactive prompt that will allow you to experiment.

### Testing packaging locally

You can use `bundle exec rake package` to generate packages locally without publishing them.

TIP: If the test that checks for permissions ("gem release process ... sets the right permissions on the gem files"), you
may need to run `umask 0022 && bundle exec rake package` so that the generated packages have the correct permissions.

## Releasing a new version to rubygems.org

Note: No Ruby needed to run this! It all runs inside docker :)

Note: Publishing new releases to rubygems.org can only be done by Datadog employees.

1. [ ] Locate the new libdatadog release on GitHub: <https://github.com/datadog/libdatadog/releases>
2. [ ] Update the `LIB_VERSION_TO_PACKAGE` and `LIB_GITHUB_RELEASES` sections of the `Rakefile` with the new version
3. [ ] Update the <lib/libdatadog/version.rb> file with the `LIB_VERSION` and `VERSION` to use
4. [ ] Commit change, open PR, get it merged
5. [ ] Release by running `docker-compose run push_to_rubygems`.
6. [ ] Verify that release shows up correctly on: <https://rubygems.org/gems/libdatadog>

## Contributing

See <../CONTRIBUTING.md>.
