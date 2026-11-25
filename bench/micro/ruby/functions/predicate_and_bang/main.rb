class Order
  def valid?
    true
  end

  def save!
    raise "invalid" unless valid?
    persist
  end

  def persist
    1
  end
end
