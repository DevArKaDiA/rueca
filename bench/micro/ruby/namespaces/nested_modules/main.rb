module App
  module Services
    class Billing
      def charge
        1
      end
    end
  end
end

def run
  App::Services::Billing.new.charge
end
