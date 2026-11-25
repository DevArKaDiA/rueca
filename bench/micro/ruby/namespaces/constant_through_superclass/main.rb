class Base
  class Helper
    def self.assist
      1
    end
  end
end

class Child < Base
  def go
    Helper.assist
  end
end
