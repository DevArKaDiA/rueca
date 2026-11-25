class Counter
  def count
    1
  end

  def run
    total = count
    count = 5
    total + count
  end
end
