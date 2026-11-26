class Animal
  def speak
    sound
  end

  def sound
    "..."
  end
end

class Dog < Animal
  def bark
    speak
  end
end
