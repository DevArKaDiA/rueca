class Conn
  def query
    1
  end
end

class Pool
  def checkout
    Conn.new
  end

  def run
    checkout.query
  end
end
