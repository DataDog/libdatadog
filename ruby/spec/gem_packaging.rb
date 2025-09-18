# Note: This file does not end with _spec on purpose, it should only be run after packaging, e.g. with `rake spec_validate_permissions`

require "rubygems"
require "rubygems/package"
require "rubygems/package/tar_reader"
require "libdatadog"
require "zlib"

RSpec.describe "gem release process (after packaging)" do
  let(:gem_version) { Libdatadog::VERSION }
  let(:packaged_gem_file) { "pkg/libdatadog-#{gem_version}.gem" }
  let(:executable_permissions) { ["libdatadog-crashtracking-receiver", "libdatadog_profiling.so"] }

  it "sets the right permissions on the .gem files" do
    gem_files = Dir.glob("pkg/*.gem")
    expect(gem_files).to include(packaged_gem_file)

    gem_files.each do |gem_file|
      Gem::Package::TarReader.new(File.open(gem_file)) do |tar|
        data = tar.find { |entry| entry.header.name == "data.tar.gz" }

        Gem::Package::TarReader.new(Zlib::GzipReader.new(StringIO.new(data.read))) do |data_tar|
          data_tar.each do |entry|
            filename = entry.header.name.split("/").last
            octal_permissions = entry.header.mode.to_s(8)[-3..-1]

            expected_permissions = executable_permissions.include?(filename) ? "755" : "644"

            expect(octal_permissions).to eq(expected_permissions),
              "Unexpected permissions for #{filename} inside #{gem_file} (got #{octal_permissions}, " \
              "expected #{expected_permissions})"
          end
        end
      end
    end
  end

  it "prefixes all public symbols in .so files" do
    so_files = Dir.glob("vendor/libdatadog-#{Libdatadog::LIB_VERSION}/**/*.so")
    expect(so_files.size).to be 4

    so_files.each do |so_file|
      raw_symbols = `nm -D --defined-only #{so_file}`

      symbols = raw_symbols.split("\n").map { |symbol| symbol.split(" ").last }.sort
      expect(symbols.size).to be > 20 # Quick sanity check

      expect(symbols).to all(
        start_with("ddog_").or(start_with("blaze_"))
      )
    end
  end
end
