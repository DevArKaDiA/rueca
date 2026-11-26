module Finders
  def find_all
    []
  end
end

class Repo
  extend Finders

  def self.everything
    find_all
  end
end

def list
  Repo.find_all
end
