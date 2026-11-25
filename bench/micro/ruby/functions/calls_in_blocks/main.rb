class Batch
  def items
    [1, 2]
  end

  def double(x)
    x * 2
  end

  def run
    items.map { |i| double(i) }.each do |v|
      log(v)
    end
  end

  def log(v)
    v
  end
end
