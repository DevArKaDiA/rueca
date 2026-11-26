class Base
  def initialize
    setup
  end

  def setup
    1
  end
end

class Child < Base
end

def make
  Child.new
end
