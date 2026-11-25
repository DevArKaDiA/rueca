class Shape
  def describe
    "area " + area.to_s
  end

  def area
    0
  end
end

class Square < Shape
  def area
    4
  end

  def show
    describe
  end
end
