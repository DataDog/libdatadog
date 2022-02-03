# frozen_string_literal: true

source "https://rubygems.org"

# Specify your gem's dependencies in libddprof.gemspec
gemspec

gem "rake", "~> 13.0"
gem "rspec", "~> 3.10"
gem "standard", "~> 1.3" unless RUBY_VERSION < "2.5"
gem "http", "~> 5.0"
gem "pry"
gem "pry-byebug"
