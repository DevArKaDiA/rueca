module Loud
  def say
    super.upcase
  end
end

class Speaker
  prepend Loud

  def say
    "hi"
  end

  def talk
    say
  end
end
