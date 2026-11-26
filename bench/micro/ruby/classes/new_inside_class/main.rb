class Token
  def initialize(v)
    @v = v
  end

  def self.parse(s)
    new(s)
  end
end
