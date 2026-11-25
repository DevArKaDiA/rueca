module Shop
  class Base
    def save
      1
    end
  end

  class Item < Base
    def store
      save
    end
  end
end
