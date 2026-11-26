module Paths
  extend self

  def root
    "/"
  end

  def join(a)
    root + a
  end
end

def run
  Paths.join("x")
end
