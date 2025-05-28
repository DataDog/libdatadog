# frozen_string_literal: true

lib = File.expand_path("../lib", __FILE__)
$LOAD_PATH.unshift(lib) unless $LOAD_PATH.include?(lib)
require "libdatadog/version"

Gem::Specification.new do |spec|
  spec.name = "libdatadog"
  spec.version = Libdatadog::VERSION
  spec.authors = ["Datadog, Inc."]
  spec.email = ["dev@datadoghq.com"]

  spec.summary = "Library of common code used by Datadog Continuous Profiler for Ruby"
  spec.description =
    "libdatadog is a Rust-based utility library for Datadog's ddtrace gem."
  spec.homepage = "https://docs.datadoghq.com/tracing/"
  spec.license = "Apache-2.0"
  spec.required_ruby_version = ">= 2.5.0"

  spec.metadata["allowed_push_host"] = "https://rubygems.org"

  spec.metadata["homepage_uri"] = spec.homepage
  spec.metadata["source_code_uri"] = "https://github.com/datadog/libdatadog/tree/main/ruby"

  # Require releases on rubygems.org to be coming from multi-factor-auth-authenticated accounts
  spec.metadata["rubygems_mfa_required"] = "true"

  # Specify which files should be added to the gem when it is released.
  # The `git ls-files -z` loads the files in the RubyGem that have been added into git.
  spec.files = Dir.chdir(File.expand_path(__dir__)) do
    `git ls-files -z`
      .split("\x0")
      .reject do |f|
        (f == __FILE__) || f.match(%r{\A(?:(?:bin|test|spec|features|publish)/|\.(?:git|travis|circleci)|appveyor)})
      end
      .reject do |f|
        [".rspec", ".standard.yml", "Rakefile", "docker-compose.yml", "gems.rb", "README.md"].include?(f)
      end
      .reject { |f| f.end_with?(".tar.gz") }
  end
  spec.require_paths = ["lib"]
end
