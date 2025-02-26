#!/usr/bin/env ruby

loop do
  # Find files matching "profile_<digits>.pprof"
  profile_files = Dir.glob("profile_*.pprof").select { |f| f =~ /\Aprofile_\d+\.pprof\z/ }.sort

  # If no matching files, sleep for 1 second and retry
  if profile_files.empty?
    puts "No profiles available"
    sleep 1
    next
  end

  profile_files.each do |profile_file|
    # Extract the numeric portion from the file name
    if profile_file =~ /\Aprofile_(\d+)\.pprof\z/
      puts "Validating #{profile_file}..."

      number = $1
      decompressed_file = "decompressed_#{number}.pprof"

      # Run lz4cat to decompress the file
      lz4_command = "lz4cat #{profile_file} > #{decompressed_file}"
      system(lz4_command)
      unless $?.exitstatus.zero?
        puts "Error: Failed to decompress #{profile_file} using lz4cat."
        exit 1
      end

      # Run go tool pprof -raw on the decompressed file
      pprof_command = "go tool pprof -raw #{decompressed_file} > /dev/null"
      system(pprof_command)
      unless $?.exitstatus.zero?
        puts "Error: 'go tool pprof' reported an error on #{decompressed_file}."
        exit 1
      end

      # If successful, delete both the profile and decompressed files
      File.delete(profile_file)
      File.delete(decompressed_file)
    end
  end
end
