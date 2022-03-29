# frozen_string_literal: true

require_relative "libddprof/version"

module Libddprof
  # This should only be used for debugging/logging
  def self.available_binaries
    File.directory?(vendor_directory) ? (Dir.entries(vendor_directory) - [".", ".."]) : []
  end

  def self.pkgconfig_folder(pkgconfig_file_name = "ddprof_ffi_with_rpath.pc")
    current_platform = Gem::Platform.local.to_s

    return unless available_binaries.include?(current_platform)

    pkgconfig_file = Dir.glob("#{vendor_directory}/#{current_platform}/**/#{pkgconfig_file_name}").first

    return unless pkgconfig_file

    File.absolute_path(File.dirname(pkgconfig_file))
  end

  private_class_method def self.vendor_directory
    ENV["LIBDDPROF_VENDOR_OVERRIDE"] || "#{__dir__}/../vendor/libddprof-#{Libddprof::LIB_VERSION}/"
  end
end
