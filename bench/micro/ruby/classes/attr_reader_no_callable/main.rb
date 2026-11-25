class Person
  attr_reader :name

  def initialize(name)
    @name = name
  end

  def greet
    "hi " + name
  end
end
