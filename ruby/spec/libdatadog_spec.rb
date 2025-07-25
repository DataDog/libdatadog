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
      rescue Errno::ENOENT, Errno::ENOTDIR
        # Do nothing, it's ok
      end
    end

    shared_examples_for "libdatadog not in usable state" do
      describe ".pkgconfig_folder" do
        it { expect(Libdatadog.pkgconfig_folder).to be nil }
      end

      describe ".path_to_crashtracking_receiver_binary" do
        it { expect(Libdatadog.path_to_crashtracking_receiver_binary).to be nil }
      end

      describe ".ld_library_path" do
        it { expect(Libdatadog.ld_library_path).to be nil }
      end
    end

    context "when no binaries are available in the vendor directory" do
      describe ".available_binaries" do
        it { expect(Libdatadog.available_binaries).to be_empty }
      end

      it_behaves_like "libdatadog not in usable state"
    end

    context "when vendor directory does not exist" do
      let(:temporary_directory) { "does/not/exist" }

      describe ".available_binaries" do
        it { expect(Libdatadog.available_binaries).to be_empty }
      end

      it_behaves_like "libdatadog not in usable state"
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
        let(:pkgconfig_folder) { "#{temporary_directory}/#{Gem::Platform.local}/some/folder/containing/the/lib/pkgconfig" }

        before do
          create_dummy_pkgconfig_file(pkgconfig_folder)
        end

        def create_dummy_pkgconfig_file(pkgconfig_folder)
          begin
            FileUtils.mkdir_p(pkgconfig_folder)
          rescue Errno::EEXIST
            # No problem, a few specs try to create the same folder
          end

          File.open("#{pkgconfig_folder}/datadog_profiling_with_rpath.pc", "w+") {}
        end

        describe ".pkgconfig_folder" do
          it "returns the folder containing the pkgconfig file" do
            expect(Libdatadog.pkgconfig_folder).to eq pkgconfig_folder
          end
        end

        context "when `RbConfig::CONFIG[\"arch\"]` indicates we're on musl libc, but `Gem::Platform.local.to_s` does not detect it" do
          # Fix for https://github.com/DataDog/dd-trace-rb/issues/2222

          before do
            allow(RbConfig::CONFIG).to receive(:[]).and_call_original
            allow(RbConfig::CONFIG).to receive(:[]).with("arch").and_return("x86_64-linux-musl")
            allow(Gem::Platform).to receive(:local).and_return("x86_64-linux")

            ["x86_64-linux", "x86_64-linux-musl"].each do |arch|
              create_dummy_pkgconfig_file("#{temporary_directory}/#{arch}/some/folder/containing/the/pkgconfig/file")
            end
          end

          it "returns the folder containing the pkgconfig file for the musl variant" do
            expect(Libdatadog.pkgconfig_folder).to eq "#{temporary_directory}/x86_64-linux-musl/some/folder/containing/the/pkgconfig/file"
          end
        end

        context "when platform ends with -gnu" do
          let(:pkgconfig_folder) { "#{temporary_directory}/aarch64-linux/some/folder/containing/the/pkgconfig/file" }

          before do
            allow(Gem::Platform).to receive(:local).and_return(Gem::Platform.new("aarch64-linux-gnu"))
          end

          it "chops off the -gnu suffix and returns the folder containing the pkgconfig file for the non-gnu variant" do
            expect(Libdatadog.pkgconfig_folder).to eq pkgconfig_folder
          end
        end

        describe ".path_to_crashtracking_receiver_binary" do
          it "returns the full path to the crashtracking_receiver_binary" do
            expect(Libdatadog.path_to_crashtracking_receiver_binary).to eq(
              "#{temporary_directory}/#{Gem::Platform.local}/some/folder/containing/the/bin/libdatadog-crashtracking-receiver"
            )
          end
        end

        describe ".ld_library_path" do
          it "returns the full path to the libdatadog lib directory" do
            expect(Libdatadog.ld_library_path).to eq(
              "#{temporary_directory}/#{Gem::Platform.local}/some/folder/containing/the/lib"
            )
          end
        end
      end

      context "but not for the current platform" do
        it_behaves_like "libdatadog not in usable state"
      end
    end
  end
end
