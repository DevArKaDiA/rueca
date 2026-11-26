require_relative "cache"

class Store
  include Cache

  def get(k)
    cached(k)
  end
end
