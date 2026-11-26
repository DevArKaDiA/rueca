require_relative "inventory"

module Shop
  class Store
    attr_reader :inventory

    def self.open
      new(Inventory.new)
    end

    def initialize(inventory)
      @inventory = inventory
      @audit = Audit.new
    end

    def sell(sku)
      @audit.record(sku)
      currency = ENV.fetch("SHOP_CURRENCY", "USD")
      [sku, currency]
    end
  end
end
