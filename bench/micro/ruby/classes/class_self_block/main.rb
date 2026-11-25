class Registry
  class << self
    def register(name)
      store << name
    end

    def store
      @store ||= []
    end
  end
end

def boot
  Registry.register(:a)
end
