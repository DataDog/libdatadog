# libddprof Ruby gem

`libddprof` provides a shared library containing common code used in the implementation of Datadog's
[Continuous Profilers](https://docs.datadoghq.com/tracing/profiler/).

**NOTE**: If you're building a new profiler or want to contribute to Datadog's existing profilers,
you've come to the right place!
Otherwise, this is possibly not the droid you were looking for.

## Development

Run `bundle exec rake` to run the tests and the style autofixer.
You can also run `bundle exec pry` for an interactive prompt that will allow you to experiment.

## Releasing a new version to rubygems.org

Note: No Ruby needed to run this! It all runs inside docker :)

Note: Publishing new releases to rubygems.org can only be done by Datadog employees.

1. [ ] Locate the new libddprof release on GitHub: <https://github.com/DataDog/libddprof/releases>
2. [ ] Update the `LIB_GITHUB_RELEASES` section of the <Rakefile> with the new version
3. [ ] Update the <lib/libddprof/version.rb> file with the `LIB_VERSION` and `VERSION` to use
4. [ ] Commit change, open PR, get it merged
5. [ ] Release by running `docker-compose run push_to_rubygems`.
    (When asked for rubygems credentials, check your local friendly Datadog 1Password.)
6. [ ] Verify that release shows up correctly on: <https://rubygems.org/gems/libddprof>

## Contributing

See <../CONTRIBUTING.md>.
