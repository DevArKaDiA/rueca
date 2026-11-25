require "shop/version"
require_relative "shop/store"

# The shop.
module Shop
  def self.store
    Store.new(Inventory.new)
  end
end
