class Cat
  def speak
    1
  end
end

class Robot
  def speak
    2
  end
end

def run(flag)
  pet = Cat.new
  pet = Robot.new if flag
  pet.speak
end
