# frozen_string_literal: true

require "tmpdir"
require "fileutils"

RSpec.describe Libdatadog do
  describe "version constants" do
    it "has a version number" do
      expect(Libdatadog::VERSION).to_not be nil
    end

    it "has an upstream libdatadog version number" do
      expect(Libdatadog::LIB_VERSION).to_not be nil
    end
  end

  describe "binary helper methods" do
    let(:temporary_directory) { Dir.mktmpdir }

    before do
      allow(ENV).to receive(:[]).and_call_original
      allow(ENV).to receive(:[]).with("LIBDATADOG_VENDOR_OVERRIDE").and_return(temporary_directory)
    end

    after do
      begin
        FileUtils.remove_dir(temporary_directory)
      rescue Errno::ENOENT => _e
        # Do nothing, it's ok
      end
    end

    context "when no binaries are available in the vendor directory" do
      describe ".available_binaries" do
        it { expect(Libdatadog.available_binaries).to be_empty }
      end

      describe ".pkgconfig_folder" do
        it { expect(Libdatadog.pkgconfig_folder).to be nil }
      end
    end

    context "when vendor directory does not exist" do
      let(:temporary_directory) { "does/not/exist" }

      describe ".available_binaries" do
        it { expect(Libdatadog.available_binaries).to be_empty }
      end

      describe ".pkgconfig_folder" do
        it { expect(Libdatadog.pkgconfig_folder).to be nil }
      end
    end

    context "when binaries are available in the vendor directory" do
      before do
        Dir.mkdir("#{temporary_directory}/386-freedos")
        Dir.mkdir("#{temporary_directory}/mipsel-linux")
      end

      describe ".available_binaries" do
        it { expect(Libdatadog.available_binaries).to contain_exactly("386-freedos", "mipsel-linux") }
      end

      context "for the current platform" do
        let(:pkgconfig_folder) { "#{temporary_directory}/#{Gem::Platform.local}/some/folder/containing/the/pkgconfig/file" }

        before do
          FileUtils.mkdir_p(pkgconfig_folder)
          File.open("#{pkgconfig_folder}/ddprof_ffi_with_rpath.pc", "w") {}
        end

        describe ".pkgconfig_folder" do
          it "returns the folder containing the pkgconfig file" do
            expect(Libdatadog.pkgconfig_folder).to eq pkgconfig_folder
          end
        end
      end

      context "but not for the current platform" do
        describe ".pkgconfig_folder" do
          it { expect(Libdatadog.pkgconfig_folder).to be nil }
        end
      end
    end
  end
end
