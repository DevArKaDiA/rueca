require "tool/version"

module Tool
  def self.banner
    Version.string
  end
end
