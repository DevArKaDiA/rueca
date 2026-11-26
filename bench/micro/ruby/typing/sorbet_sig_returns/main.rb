# typed: true
class Conn
  def query
    1
  end
end

class Pool
  extend T::Sig

  sig { returns(Conn) }
  def checkout
    Conn.new
  end

  def run
    checkout.query
  end
end
