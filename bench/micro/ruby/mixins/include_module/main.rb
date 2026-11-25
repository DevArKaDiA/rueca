module Greeting
  def greet
    "hi " + name
  end
end

class Person
  include Greeting

  def name
    "p"
  end

  def hello
    greet
  end
end
