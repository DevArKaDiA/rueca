class Base
  def run(x)
    x
  end
end

class Child < Base
  def run(x)
    super(x) + 1
  end
end
