# frozen_string_literal: true

require_relative "libddprof/version"

module Libddprof
  # Does this libddprof release include any binaries?
  def self.binaries?
    available_binaries.any?
  end

  # This should only be used for debugging/logging
  def self.available_binaries
    File.directory?(vendor_directory) ? (Dir.entries(vendor_directory) - [".", ".."]) : []
  end

  def self.pkgconfig_folder
    current_platform = Gem::Platform.local.to_s

    return unless available_binaries.include?(current_platform)

    pkgconfig_file = Dir.glob("#{vendor_directory}/#{current_platform}/**/ddprof_ffi.pc").first

    return unless pkgconfig_file

    File.absolute_path(File.dirname(pkgconfig_file))
  end

  private_class_method def self.vendor_directory
    ENV["LIBDDPROF_VENDOR_OVERRIDE"] || "#{__dir__}/../vendor/libddprof-#{Libddprof::LIB_VERSION}/"
  end
end
