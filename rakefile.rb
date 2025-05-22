# This file is used so that the Ruby `rake` command can be used from the root of the repository.
# This is needed for the publish-ruby.yml CI workflow to work.

require "rake"

Dir.chdir("ruby")
Rake.application.add_import("Rakefile")
Rake.application.load_imports
