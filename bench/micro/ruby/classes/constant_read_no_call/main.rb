class Limits
  MAX = 3

  def self.max
    MAX
  end
end

def check
  Limits::MAX + Limits.max
end
