module Version
  def self.current
    parse("1")
  end

  def self.parse(s)
    s
  end
end

def show
  Version.current
end
