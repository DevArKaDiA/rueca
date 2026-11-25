class Config
  def self.load(path)
    parse(path)
  end

  def self.parse(path)
    path
  end
end

def main
  Config.load("x")
end
