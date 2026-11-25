class Base
  def initialize(a)
    @a = a
  end
end

class Child < Base
  def initialize(a, b)
    super(a)
    @b = b
  end
end

def make
  Child.new(1, 2)
end
