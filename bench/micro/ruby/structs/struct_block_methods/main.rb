Pair = Struct.new(:left, :right) do
  def sum
    left + right
  end

  def describe
    "sum " + sum.to_s
  end
end
