module Shop
  module Logger
    module_function

    def info(message)
      format(message)
    end

    def format(message)
      "[shop] #{message}"
    end
  end
end
