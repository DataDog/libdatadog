# frozen_string_literal: true

source "https://rubygems.org"

# Specify your gem's dependencies in libdatadog.gemspec
gemspec

gem "rake", ">= 12.0", "< 14"
gem "rspec", "~> 3.10"
gem "standard", "~> 1.7", ">= 1.7.2" unless RUBY_VERSION < "2.6"
gem "http", "~> 5.0" unless RUBY_VERSION < "2.5"
gem "pry"
gem "pry-byebug" unless RUBY_VERSION > "3.1"
gem "rubygems-await"
