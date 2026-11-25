module Shop
  class Inventory
    include Countable

    def initialize
      @items = {}
    end

    def add(sku, qty)
      @items[sku] = qty
      log(sku)
    end

    def count
      total
    end

    private

    def log(sku)
      Logger.info(sku)
    end
  end
end
